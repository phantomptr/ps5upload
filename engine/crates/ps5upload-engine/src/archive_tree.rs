//! Convert reading a game straight from a `.zip`, `.7z` or `.rar` on this computer (a
//! multi-part RAR set from its first part), nothing unpacked to disk: the game folder inside is
//! a [`SourceTree`] whose files come out of the archive as a build asks for them
//! (`ps5upload_convert_core::archive_tree`, shared with the converter on the PS5, over
//! `ps5upload_core::archive_stream`, which adds `.rar`).
//!
//! What can't be read this way is refused with [`NEEDS_UNPACK`] at the start of its message,
//! and the app unpacks that archive first as before: a game image or package inside an archive
//! (its reader seeks all over it), and a build that reads its source by path again (a PS4
//! update merged in, zstd packs in a folder, saving one entry).
//!
//! The password (RAR only) goes to the reader and nowhere else: never a log line, a job record
//! or an event.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use ps5upload_core::archive_stream::{self, Kind};
use ps5upload_fpkg::source::SourceTree;

/// The start of the refusal for an archive that has to be unpacked first. The app sees it and
/// takes the unpack path.
pub(crate) const NEEDS_UNPACK: &str = ps5upload_convert_core::archive_tree::NEEDS_UNPACK;

/// Whether `path` is an archive on this computer that a build reads in place.
pub(crate) fn reads_in_place(path: &Path) -> bool {
    !ps5upload_fpkg::remote_source::is_remote(path) && Kind::of(path).is_some() && path.is_file()
}

/// Whether `path` is one part of a multi-part RAR set (`x.part1.rar`, or `x.rar` with `x.r00`
/// beside it): removing it alone would leave the other parts.
pub(crate) fn multi_part(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let numbered = lower
        .strip_suffix(".rar")
        .and_then(|s| s.rsplit_once(".part"))
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if numbered {
        return true;
    }
    let Some(stem) = lower.strip_suffix(".rar") else {
        return false;
    };
    let stem_len = stem.len();
    ["r00", "R00"].iter().any(|ext| {
        path.with_file_name(format!("{}.{ext}", &name[..stem_len]))
            .exists()
    })
}

/// What an image or folder made from the archive is called by default: its name without the
/// extension or `.partN` (`Game.part1.rar` → `Game`), with no trailing dot or space.
pub(crate) fn stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let mut cut = name.len();
    for ext in [".zip", ".7z", ".rar"] {
        if lower.ends_with(ext) {
            cut -= ext.len();
            break;
        }
    }
    if let Some((stem, n)) = lower[..cut].rsplit_once(".part") {
        if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
            cut = stem.len();
        }
    }
    let stem = name[..cut].trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        "game".to_string()
    } else {
        crate::image_build::stem_of(Path::new(stem))
    }
}

/// A build's archive source: its password (in memory only) and the job's cancel flag once
/// there is a job. No `Debug`: it holds the password.
#[derive(Clone, Default)]
pub(crate) struct ArchiveSource {
    pub(crate) password: Option<String>,
    pub(crate) cancel: CancelLink,
}

/// The job's cancel flag, known only once the job exists (the tree is opened before): a read
/// waiting on the archive stops when it is set.
#[derive(Clone, Default)]
pub(crate) struct CancelLink(Arc<OnceLock<Arc<AtomicBool>>>);

impl CancelLink {
    pub(crate) fn set(&self, flag: Arc<AtomicBool>) {
        let _ = self.0.set(flag);
    }

    fn stopped(&self) -> bool {
        self.0.get().is_some_and(|f| f.load(Ordering::Relaxed))
    }
}

/// The game folder in the archive at `path`, read in place
/// (`ps5upload_convert_core::archive_tree::open_listed`). Its `param.json` (or `param.sfo`)
/// is read now, so a wrong password or a damaged archive is refused before any job starts.
pub(crate) fn open(
    path: &Path,
    password: Option<&str>,
    cancel: &CancelLink,
) -> Result<Box<dyn SourceTree>, String> {
    Kind::of(path).ok_or_else(|| format!("{} is not a .zip, .7z or .rar", path.display()))?;
    let listing = archive_stream::list(path, password).map_err(|e| format!("{e:#}"))?;
    let link = cancel.clone();
    let lane: archive_stream::Lane = Arc::new(|| {
        // Taken on the reading thread (the job's), followed on the archive's.
        let lane = ps5upload_fpkg::workshop::Lane::current();
        let mut entered = false;
        Box::new(move || {
            if !entered {
                lane.enter();
                entered = true;
            }
            ps5upload_fpkg::workshop::sync_thread_priority();
        })
    });
    let tree = ps5upload_convert_core::archive_tree::open_listed(
        path,
        password,
        listing,
        Arc::new(move || link.stopped()),
        Some(lane),
    )?;
    Ok(Box::new(tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multi_part_set_is_told_by_name() {
        assert!(multi_part(Path::new("/x/Game.part1.rar")));
        assert!(multi_part(Path::new("/x/Game.PART01.RAR")));
        assert!(!multi_part(Path::new("/nonexistent-dir/Game.rar")));
        assert!(!multi_part(Path::new("/x/Game.zip")));
    }
}
