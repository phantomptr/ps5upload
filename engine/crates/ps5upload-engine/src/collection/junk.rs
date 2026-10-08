//! Finder's leftovers in a games folder: AppleDouble sidecars (`._name`), `.DS_Store`,
//! `.localized` and `Icon\r`. A port of PS Game Library's `sidecars.py` and its "Clean macOS
//! junk", for every OS: an exFAT drive written by a Mac carries them to whichever computer it
//! is plugged into next.
//!
//! A `._` file counts as a sidecar only when its first bytes are the AppleDouble signature;
//! a file merely named `._x` is someone's data and is left alone. Symlinks are never followed
//! or removed.

use std::path::{Path, PathBuf};

use serde::Serialize;

const APPLEDOUBLE_MAGIC: [u8; 4] = [0x00, 0x05, 0x16, 0x07];
/// A sidecar holds attributes, not data: anything this large is not one.
const MAX_SIDECAR_BYTES: u64 = 16 * 1024 * 1024;

/// Finder's names, before any check of the file itself.
pub fn is_junk_name(name: &str) -> bool {
    name.starts_with("._") || name == ".DS_Store" || name == ".localized" || name == "Icon\r"
}

/// A regular `._` file carrying the AppleDouble signature.
pub fn is_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("._") || name == "._" {
        return false;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() > MAX_SIDECAR_BYTES {
        return false;
    }
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
        .is_ok()
        && head == APPLEDOUBLE_MAGIC
}

/// A file Finder made that can go: a verified sidecar, or one of its fixed names.
pub fn is_removable_junk(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with("._") {
        return is_sidecar(path);
    }
    is_junk_name(name)
        && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
}

/// Bytes a file occupies on disk, whole clusters included.
fn allocated(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let blocks = meta.blocks() * 512;
        if blocks > 0 {
            return blocks;
        }
    }
    meta.len()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JunkFile {
    pub path: String,
    pub size: u64,
    /// Space on disk, which on a 256 KB-cluster exFAT drive is far more than `size`.
    pub allocated: u64,
    pub sidecar: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct JunkReport {
    pub files: Vec<JunkFile>,
    pub checked: u64,
    pub allocated: u64,
    pub cancelled: bool,
}

/// Every junk file below `root`, symlinks unfollowed. `stop` is polled as it walks.
pub fn find(root: &Path, stop: &dyn Fn() -> bool) -> JunkReport {
    let mut report = JunkReport::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if stop() {
            report.cancelled = true;
            return report;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            report.checked += 1;
            if ft.is_dir() {
                // A sidecar can describe a directory, but never is one.
                if !name.starts_with("._") {
                    stack.push(path);
                }
                continue;
            }
            if !is_junk_name(&name) || !is_removable_junk(&path) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let file = JunkFile {
                path: path.to_string_lossy().into_owned(),
                size: meta.len(),
                allocated: allocated(&meta),
                sidecar: name.starts_with("._"),
            };
            report.allocated += file.allocated;
            report.files.push(file);
        }
    }
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanResult {
    pub removed: u64,
    pub freed: u64,
    pub failed: Vec<String>,
}

/// Remove what `find` reported, each file checked again at the moment of deletion in case it
/// changed since. Junk holds no data, so it is deleted rather than sent to a trash.
pub fn remove(files: &[PathBuf]) -> CleanResult {
    let mut out = CleanResult::default();
    for path in files {
        if !is_removable_junk(path) {
            continue;
        }
        let freed = std::fs::symlink_metadata(path)
            .map(|m| allocated(&m))
            .unwrap_or(0);
        match std::fs::remove_file(path) {
            Ok(()) => {
                out.removed += 1;
                out.freed += freed;
            }
            Err(e) => out.failed.push(format!("{}: {e}", path.display())),
        }
    }
    out
}

/// The sidecars directly inside `dir` and its subfolders, removed: the "after each scan"
/// sweep. Sidecars only; Finder's other files are left for an explicit clean-up.
pub fn sweep_sidecars(root: &Path) -> CleanResult {
    let found = find(root, &|| false);
    let sidecars: Vec<PathBuf> = found
        .files
        .into_iter()
        .filter(|f| f.sidecar)
        .map(|f| PathBuf::from(f.path))
        .collect();
    remove(&sidecars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5u-junk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_a_signed_dot_underscore_file_is_a_sidecar() {
        let d = tmp("sig");
        std::fs::write(d.join("._game.pkg"), [0x00, 0x05, 0x16, 0x07, 1, 2]).unwrap();
        std::fs::write(d.join("._notes.txt"), b"my own file").unwrap();
        std::fs::write(d.join("._"), [0x00, 0x05, 0x16, 0x07]).unwrap();
        assert!(is_sidecar(&d.join("._game.pkg")));
        assert!(!is_sidecar(&d.join("._notes.txt")));
        assert!(!is_sidecar(&d.join("._")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn find_reports_finder_files_and_leaves_everything_else() {
        let d = tmp("find");
        std::fs::create_dir_all(d.join("PS4/Game")).unwrap();
        std::fs::write(d.join("PS4/Game/game.pkg"), b"pkg").unwrap();
        std::fs::write(d.join("PS4/Game/._game.pkg"), [0x00, 0x05, 0x16, 0x07]).unwrap();
        std::fs::write(d.join("PS4/.DS_Store"), b"x").unwrap();
        std::fs::write(d.join("Icon\r"), b"").unwrap();
        std::fs::write(d.join("._mine"), b"not a sidecar").unwrap();
        let r = find(&d, &|| false);
        let names: Vec<String> = r
            .files
            .iter()
            .map(|f| {
                Path::new(&f.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(!names.contains(&"._mine".to_string()));
        let out = remove(
            &r.files
                .iter()
                .map(|f| PathBuf::from(&f.path))
                .collect::<Vec<_>>(),
        );
        assert_eq!(out.removed, 3);
        assert!(d.join("PS4/Game/game.pkg").exists() && d.join("._mine").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_after_scan_sweep_takes_sidecars_only() {
        let d = tmp("sweep");
        std::fs::write(d.join("._a.pkg"), [0x00, 0x05, 0x16, 0x07]).unwrap();
        std::fs::write(d.join(".DS_Store"), b"x").unwrap();
        let out = sweep_sidecars(&d);
        assert_eq!(out.removed, 1);
        assert!(d.join(".DS_Store").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
