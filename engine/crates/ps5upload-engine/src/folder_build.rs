//! A game folder from a game image or a debug-key package (#469): the reverse of Convert's
//! image and package builds.
//!
//! The rules the image builds follow hold here too. Every path is checked before a byte is
//! written; the folder is written as `<name>.partial/` and only renamed to its name once it
//! reads back; and the read-back compares hashes (SHA-256), not only names and sizes: of every
//! file on request (full), of a seeded sample otherwise (`ps5upload_fpkg::sample`). The sample
//! is re-read from a freshly opened source, so a reader that returned the wrong bytes the first
//! time is not trusted to return the same wrong bytes again.
//!
//! A `.partial` folder carries [`MARKER`] while it is written, which is how the output folder's
//! leftover sweep tells our unfinished folders from a user's own `something.partial`.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::sample::{self, VerifyMode, VerifySummary};
use ps5upload_fpkg::source::SourceTree;
use sha2::{Digest, Sha256};

/// How much of a file is read from the source per call while writing and verifying.
pub(crate) const CHUNK: u64 = 8 << 20;

/// Room a folder build keeps beyond the files themselves (directory entries, the marker).
pub(crate) const SPARE: u64 = 64 << 20;

/// The file inside a `<name>.partial/` folder that says this engine is writing it.
pub(crate) const MARKER: &str = ".ps5upload-partial";

/// The unfinished folder a build of `target` writes into.
pub(crate) fn partial_of(target: &Path) -> PathBuf {
    PathBuf::from(format!("{}.partial", target.display()))
}

/// The first of `stem`, `stem (2)`, `stem (3)`, … that `taken` says is free: how a build asked
/// to keep both (Convert's "Save each next to its source") names its output when the name is
/// already used, instead of refusing. `None` when every try up to `(999)` is taken.
pub(crate) fn keep_both_stem(stem: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    if !taken(stem) {
        return Some(stem.to_string());
    }
    (2..=999)
        .map(|n| format!("{stem} ({n})"))
        .find(|candidate| !taken(candidate))
}

/// True when `path` or its `.partial` (an unfinished build's) exists: the name is in use.
pub(crate) fn name_in_use(path: &Path) -> bool {
    path.exists() || partial_of(path).exists()
}

/// True when `dir` is an unfinished folder build of ours (it holds [`MARKER`]).
pub(crate) fn is_our_partial_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".partial"))
        && dir.join(MARKER).is_file()
}

/// The refusal a folder of `total` bytes deserves with `free` bytes left in `out`, or `None`.
pub(crate) fn shortfall(free: u64, total: u64, out: &Path) -> Option<String> {
    let needed = total.saturating_add(SPARE);
    (free < needed).then(|| {
        format!(
            "only {:.1} GiB free in {}, and this needs about {:.1} GiB",
            free as f64 / (1u64 << 30) as f64,
            out.display(),
            needed as f64 / (1u64 << 30) as f64,
        )
    })
}

/// What a finished folder build reports.
#[derive(Debug)]
pub(crate) struct FolderBuilt {
    pub files: u64,
    pub bytes: u64,
    pub detail: String,
}

/// Why one name cannot be a file or folder name on `windows` (or on a Unix system), or `None`.
fn bad_component(name: &str, windows: bool) -> Option<&'static str> {
    if name.is_empty() {
        return Some("has an empty name in its path");
    }
    if name == "." || name == ".." {
        return Some("points outside its folder");
    }
    if name.contains('\0') {
        return Some("has a NUL character in its name");
    }
    if name.len() > 255 {
        return Some("has a name longer than 255 bytes");
    }
    if windows {
        if name
            .chars()
            .any(|c| (c as u32) < 0x20 || "<>:\"\\|?*".contains(c))
        {
            return Some("has a character Windows does not allow in a name (<>:\"\\|?* or a control character)");
        }
        if name.ends_with('.') || name.ends_with(' ') {
            return Some("ends with a dot or space, which Windows drops");
        }
        let base = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((base.starts_with("COM") || base.starts_with("LPT"))
                && base.len() == 4
                && base.as_bytes()[3].is_ascii_digit()
                && base.as_bytes()[3] != b'0');
        if reserved {
            return Some("has a name Windows reserves for a device (CON, NUL, COM1, …)");
        }
    }
    None
}

/// Why `path` (relative, `/`-separated) cannot be written under a folder, or `None`.
pub(crate) fn path_problem(path: &str, windows: bool) -> Option<&'static str> {
    if path.starts_with('/') || path.starts_with('\\') {
        return Some("is an absolute path");
    }
    if windows && path.as_bytes().get(1) == Some(&b':') {
        return Some("is an absolute path");
    }
    path.split('/').find_map(|c| bad_component(c, windows))
}

/// Why a folder name a caller gave cannot be used, or `None`.
pub(crate) fn name_problem(name: &str) -> Option<&'static str> {
    if name.contains('/') || name.contains('\\') {
        return Some("has a path separator in it");
    }
    if name.ends_with(".partial") {
        return Some("ends with .partial, which marks an unfinished folder");
    }
    bad_component(name, cfg!(windows))
}

/// Every path checked before anything is written: each must be a name this OS can hold, no
/// file may also be a folder, and on a case-insensitive drive (`fold_case`) no two may differ
/// only by case. The error names the file.
fn check_paths(tree: &dyn SourceTree, fold_case: bool) -> Result<(), String> {
    let windows = cfg!(windows);
    let mut seen: HashSet<String> = HashSet::new();
    let mut dirs: HashSet<String> = HashSet::new();
    let key = |p: &str| {
        if fold_case {
            p.to_lowercase()
        } else {
            p.to_string()
        }
    };
    let mut all: Vec<(&str, bool)> = tree
        .files()
        .iter()
        .map(|f| (f.path.as_str(), true))
        .collect();
    all.extend(
        tree.empty_dirs()
            .iter()
            .map(|d| (d.trim_end_matches('/'), false)),
    );
    for (path, _) in &all {
        if let Some(why) = path_problem(path, windows) {
            return Err(format!("{path} {why}; nothing was written"));
        }
        if path.split('/').next() == Some(MARKER) {
            return Err(format!(
                "{path} has the name this app uses to mark an unfinished folder; nothing was written"
            ));
        }
    }
    for (path, is_file) in &all {
        let mut at = String::new();
        for c in path.split('/').take(path.split('/').count() - 1) {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(c);
            dirs.insert(key(&at));
        }
        if !is_file {
            dirs.insert(key(path));
        }
    }
    for (path, is_file) in &all {
        if !is_file {
            continue;
        }
        let k = key(path);
        if dirs.contains(&k) {
            return Err(format!(
                "{path} is both a file and a folder in the source; nothing was written"
            ));
        }
        if !seen.insert(k) {
            return Err(format!(
                "{path} appears twice{}; nothing was written",
                if fold_case {
                    " (names that differ only by case are one name on this drive)"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

/// The file names on the drive under `dir` ignore case: `MARKER` (just written there) can be
/// opened by its upper-case name.
fn drive_folds_case(dir: &Path) -> bool {
    dir.join(MARKER.to_ascii_uppercase()).exists()
}

/// Copies `path` out of `tree` into `dest`, `CHUNK` bytes at a time.
fn copy_file(
    tree: &mut dyn SourceTree,
    path: &str,
    size: u64,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| format!("{path}: {e}"))?;
    let mut at = 0u64;
    while at < size {
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let want = CHUNK.min(size - at) as usize;
        let bytes = tree
            .read_range(path, at, want)
            .map_err(|e| format!("{path}: {e}"))?;
        if bytes.is_empty() {
            return Err(format!("{path}: the source ended at byte {at} of {size}"));
        }
        let bytes = &bytes[..bytes.len().min(want)];
        out.write_all(bytes).map_err(|e| format!("{path}: {e}"))?;
        at += bytes.len() as u64;
        progress(bytes.len() as u64);
    }
    out.flush().map_err(|e| format!("{path}: {e}"))?;
    Ok(())
}

/// SHA-256 of `len` bytes at `offset` of `path` in `tree`. `read` is told each chunk's size, so
/// a large file moves the bar while it is read rather than only when it ends (#669).
fn hash_source(
    tree: &mut dyn SourceTree,
    path: &str,
    offset: u64,
    len: u64,
    cancel: &AtomicBool,
    read: &mut dyn FnMut(u64),
) -> Result<[u8; 32], String> {
    let mut h = Sha256::new();
    let mut at = 0u64;
    while at < len {
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let want = CHUNK.min(len - at) as usize;
        let bytes = tree
            .read_range(path, offset + at, want)
            .map_err(|e| format!("{path}: {e}"))?;
        if bytes.is_empty() {
            return Err(format!("{path}: the source ended at byte {}", offset + at));
        }
        let bytes = &bytes[..bytes.len().min(want)];
        h.update(bytes);
        at += bytes.len() as u64;
        read(bytes.len() as u64);
    }
    Ok(h.finalize().into())
}

/// SHA-256 of `len` bytes at `offset` of a written file. `read` is told each chunk's size.
fn hash_written(
    file: &Path,
    offset: u64,
    len: u64,
    cancel: &AtomicBool,
    read: &mut dyn FnMut(u64),
) -> Result<[u8; 32], String> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK.min(len.max(1)) as usize];
    let mut left = len;
    while left > 0 {
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let n = buf.len().min(left as usize);
        f.read_exact(&mut buf[..n])
            .map_err(|e| format!("{}: {e}", file.display()))?;
        h.update(&buf[..n]);
        left -= n as u64;
        read(n as u64);
    }
    Ok(h.finalize().into())
}

/// Progress through one piece of the read-back: the piece is read twice (from the source, then
/// from what was written), so each side moves the bar half as far, and `done` only reaches
/// the end of the piece once both are read. Without this a large file showed nothing until it
/// ended, and the speed fell to nothing while the disk was busy (#669).
struct BothSides<'a> {
    base: u64,
    total: u64,
    read: u64,
    progress: &'a mut dyn FnMut(u64, u64),
}

impl<'a> BothSides<'a> {
    fn new(base: u64, total: u64, progress: &'a mut dyn FnMut(u64, u64)) -> Self {
        Self {
            base,
            total,
            read: 0,
            progress,
        }
    }

    fn read(&mut self, n: u64) {
        self.read += n;
        (self.progress)(self.base + self.read / 2, self.total);
    }
}

/// Reads the written folder back: every file at its size and every empty folder present, then
/// the SHA-256 of what `mode` samples, the source side read through `fresh` (a newly opened
/// source).
fn verify(
    written: &Path,
    listed: &[(String, u64)],
    empty_dirs: &[String],
    fresh: &mut dyn SourceTree,
    mode: VerifyMode,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<VerifySummary, String> {
    for (path, size) in listed {
        let got = std::fs::metadata(written.join(path))
            .map_err(|e| format!("{path} did not read back: {e}"))?
            .len();
        if got != *size {
            return Err(format!(
                "{path} is {got} bytes in the folder and {size} in the source"
            ));
        }
    }
    for d in empty_dirs {
        let d = d.trim_matches('/');
        if !written.join(d).is_dir() {
            return Err(format!("the empty folder {d} is missing"));
        }
    }
    let summary = sample::summary(listed, mode);
    let pieces: Vec<(usize, u64, u64)> = match mode {
        VerifyMode::Full => listed
            .iter()
            .enumerate()
            .filter(|(_, f)| f.1 > 0)
            .map(|(i, f)| (i, 0, f.1))
            .collect(),
        VerifyMode::Fast { seed, seam } => sample::plan(listed, seed, seam)
            .into_iter()
            .map(|s| {
                let (at, len) = s.range(listed[s.file_idx].1);
                (s.file_idx, at, len)
            })
            .collect(),
    };
    let total = summary.checked_bytes;
    let mut done = 0u64;
    progress(0, total);
    for (i, at, len) in pieces {
        let path = &listed[i].0;
        let (want, got) = {
            let mut sides = BothSides::new(done, total, &mut *progress);
            let want = hash_source(fresh, path, at, len, cancel, &mut |n| sides.read(n))?;
            let got = hash_written(&written.join(path), at, len, cancel, &mut |n| sides.read(n))?;
            (want, got)
        };
        if want != got {
            let range = if len != listed[i].1 {
                format!(" (bytes {at}..{})", at + len)
            } else {
                String::new()
            };
            return Err(format!(
                "{path} differs between the folder and the source{range}"
            ));
        }
        done += len;
        progress(done, total);
    }
    Ok(summary)
}

/// Writes `tree` as the folder `target`: checked, written into `<target>.partial/`, read back
/// against a source `reopen` opens afresh, then renamed. `stage(id, done, total)` reports
/// `write` and `verify`. The caller has claimed the `.partial` (`fpkg_api::Writing`); on any
/// failure it is removed here.
pub(crate) fn build(
    tree: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    target: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<FolderBuilt, String> {
    // A libSceAmpr game gets a fresh ampr_emu.index, as an image and a package do (#449): a stale
    // one crashes the game. The read-back makes the same index from the source opened again.
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // A PS4 game's folder needs the param.json ShadowMount+ reads, as its image does. The
    // read-back adds the same file to the source opened again.
    let mut ps4 =
        ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *tree).map_err(|e| e.to_string())?;
    let tree: &mut dyn SourceTree = match ps4.as_mut() {
        Some(p) => p,
        None => tree,
    };
    if let Some(mut indexed) = crate::image_build::AmprIndexed::wrap_at(&mut *tree, mtime)? {
        return build_tree(
            &mut indexed,
            reopen,
            Some(mtime),
            target,
            mode,
            cancel,
            stage,
        );
    }
    build_tree(tree, reopen, None, target, mode, cancel, stage)
}

#[allow(clippy::too_many_arguments)]
fn build_tree(
    tree: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    index_mtime: Option<i64>,
    target: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<FolderBuilt, String> {
    write_checked(
        tree,
        target,
        cancel,
        stage,
        &mut |partial, listed, empty_dirs, stage| {
            let mut fresh = reopen()?;
            let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *fresh)
                .map_err(|e| e.to_string())?;
            let plain: &mut dyn SourceTree = match ps4.as_mut() {
                Some(p) => p,
                None => &mut *fresh,
            };
            let mut indexed = match index_mtime {
                Some(m) => crate::image_build::AmprIndexed::wrap_at(&mut *plain, m)?,
                None => None,
            };
            let source: &mut dyn SourceTree = match indexed.as_mut() {
                Some(i) => i,
                None => plain,
            };
            let summary = verify(
                partial,
                listed,
                empty_dirs,
                source,
                mode,
                cancel,
                &mut |d, t| stage("verify", d, t),
            )?;
            Ok(describe(&summary))
        },
    )
}

/// How a read-back went, for the job's detail.
fn describe(summary: &VerifySummary) -> String {
    match summary.mode {
        VerifyMode::Full => "SHA-256 checked in full".to_string(),
        VerifyMode::Fast { .. } => summary.describe(),
    }
}

/// The read-back of a folder [`write_checked`] wrote: given the `.partial` folder, the files
/// written (path, size), the empty folders and the stage reporter, the check's description or
/// why it failed.
pub(crate) type Check<'a> = dyn FnMut(
        &Path,
        &[(String, u64)],
        &[String],
        &mut dyn FnMut(&str, u64, u64),
    ) -> Result<String, String>
    + 'a;

/// Writes `tree` as the folder `target`: its paths checked, written into `<target>.partial/`,
/// `check`ed there, then renamed. `stage` reports `write` (and whatever `check` reports). The
/// caller has claimed the `.partial`; on any failure it is removed here.
pub(crate) fn write_checked(
    tree: &mut dyn SourceTree,
    target: &Path,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    check: &mut Check,
) -> Result<FolderBuilt, String> {
    if tree.files().is_empty() {
        return Err(format!("{} has no files in it", tree.describe()));
    }
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    // Checked before the partial folder exists; the case check needs the drive, below.
    check_paths(tree, false)?;
    let partial = partial_of(target);
    // Not `create_dir_all`: a folder here now is another build's, never written into.
    std::fs::create_dir(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let result = (|| -> Result<FolderBuilt, String> {
        std::fs::write(partial.join(MARKER), b"").map_err(|e| e.to_string())?;
        if drive_folds_case(&partial) {
            check_paths(tree, true)?;
        }
        let listed: Vec<(String, u64)> = tree
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let empty_dirs: Vec<String> = tree.empty_dirs().to_vec();
        let total: u64 = listed.iter().map(|f| f.1).sum();

        let mut written = 0u64;
        stage("write", 0, total);
        for d in &empty_dirs {
            std::fs::create_dir_all(partial.join(d.trim_matches('/')))
                .map_err(|e| format!("{d}: {e}"))?;
        }
        for (path, size) in &listed {
            let dest = partial.join(path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{path}: {e}"))?;
            }
            copy_file(tree, path, *size, &dest, cancel, &mut |n| {
                written += n;
                stage("write", written, total);
            })?;
        }
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }

        let checked = check(&partial, &listed, &empty_dirs, stage)?;
        std::fs::remove_file(partial.join(MARKER)).map_err(|e| e.to_string())?;
        if target.exists() {
            return Err(format!(
                "{} appeared while it was written",
                target.display()
            ));
        }
        std::fs::rename(&partial, target).map_err(|e| e.to_string())?;
        Ok(FolderBuilt {
            files: listed.len() as u64,
            bytes: total,
            detail: format!(
                "{} files, {} empty folders, {total} bytes, {checked}",
                listed.len(),
                empty_dirs.len(),
            ),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    match result {
        Err(_) if cancel.load(Ordering::Relaxed) => Err("cancelled".into()),
        r => r,
    }
}

/// Compares two trees: `got` must hold exactly `want`'s files at their sizes (a file named
/// [`MARKER`] in `got` aside), and the SHA-256 of what `mode` samples (or of every byte) must
/// match. `progress(done, total)` as [`verify`] reports it. The description of the check.
pub(crate) fn verify_trees(
    got: &mut dyn SourceTree,
    want: &mut dyn SourceTree,
    mode: VerifyMode,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<String, String> {
    let mut listed: Vec<(String, u64)> = want
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    listed.sort();
    let mut have: Vec<(String, u64)> = got
        .files()
        .iter()
        .filter(|f| f.path != MARKER)
        .map(|f| (f.path.clone(), f.size))
        .collect();
    have.sort();
    if have != listed {
        // Both sorted: a binary search each, not a scan of one list per entry of the other
        // (hundreds of thousands of files).
        let missing = listed.iter().find(|f| have.binary_search(f).is_err());
        let extra = have.iter().find(|f| listed.binary_search(f).is_err());
        return Err(match (missing, extra) {
            (Some((p, s)), _) => format!("{p} ({s} bytes) did not read back"),
            (None, Some((p, s))) => format!("{p} ({s} bytes) read back but is not in the source"),
            (None, None) => "the files read back differ from the source".to_string(),
        });
    }
    let summary = sample::summary(&listed, mode);
    let pieces: Vec<(usize, u64, u64)> = match mode {
        VerifyMode::Full => listed
            .iter()
            .enumerate()
            .filter(|(_, f)| f.1 > 0)
            .map(|(i, f)| (i, 0, f.1))
            .collect(),
        VerifyMode::Fast { seed, seam } => sample::plan(&listed, seed, seam)
            .into_iter()
            .map(|s| {
                let (at, len) = s.range(listed[s.file_idx].1);
                (s.file_idx, at, len)
            })
            .collect(),
    };
    let total = summary.checked_bytes;
    let mut done = 0u64;
    progress(0, total);
    for (i, at, len) in pieces {
        let path = &listed[i].0;
        let (a, b) = {
            let mut sides = BothSides::new(done, total, &mut *progress);
            let a = hash_source(want, path, at, len, cancel, &mut |n| sides.read(n))?;
            let b = hash_source(got, path, at, len, cancel, &mut |n| sides.read(n))?;
            (a, b)
        };
        if a != b {
            return Err(format!(
                "{path} (bytes {at}..{}) reads back differently from the source",
                at + len
            ));
        }
        done += len;
        progress(done, total);
    }
    Ok(describe(&summary))
}

/// Saves the one file `path` (`size` bytes) of `tree` as the file `target` (#479, the image
/// browser's Save): written as `<target>.partial`, its SHA-256 compared in full with the same
/// file read from a source `reopen` opens afresh, then renamed. `stage` reports `write` and
/// `verify`. The caller has claimed the `.partial`; on any failure it is removed here.
pub(crate) fn extract_file(
    tree: &mut dyn SourceTree,
    path: &str,
    size: u64,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    target: &Path,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<FolderBuilt, String> {
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    let partial = partial_of(target);
    let result = (|| -> Result<FolderBuilt, String> {
        let mut written = 0u64;
        stage("write", 0, size);
        copy_file(tree, path, size, &partial, cancel, &mut |n| {
            written += n;
            stage("write", written, size);
        })?;
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let got_len = std::fs::metadata(&partial)
            .map_err(|e| format!("{path} did not read back: {e}"))?
            .len();
        if got_len != size {
            return Err(format!(
                "{path} is {got_len} bytes on this computer and {size} in the image"
            ));
        }
        stage("verify", 0, size);
        let mut fresh = reopen()?;
        let (want, got) = {
            let mut report = |d: u64, t: u64| stage("verify", d, t);
            let mut sides = BothSides::new(0, size, &mut report);
            let want = hash_source(&mut *fresh, path, 0, size, cancel, &mut |n| sides.read(n))?;
            let got = hash_written(&partial, 0, size, cancel, &mut |n| sides.read(n))?;
            (want, got)
        };
        if want != got {
            return Err(format!("{path} differs between the copy and the image"));
        }
        stage("verify", size, size);
        if target.exists() {
            return Err(format!(
                "{} appeared while it was written",
                target.display()
            ));
        }
        std::fs::rename(&partial, target).map_err(|e| e.to_string())?;
        Ok(FolderBuilt {
            files: 1,
            bytes: size,
            detail: format!("1 file, {size} bytes, SHA-256 checked in full"),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    match result {
        Err(_) if cancel.load(Ordering::Relaxed) => Err("cancelled".into()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_build::{self, ImageFormat};
    use ps5upload_fpkg::source::{FolderSource, SourceFile};

    #[test]
    fn keep_both_numbers_a_taken_name() {
        assert_eq!(keep_both_stem("Game", |_| false).as_deref(), Some("Game"));
        let used = ["Game", "Game (2)"];
        assert_eq!(
            keep_both_stem("Game", |s| used.contains(&s)).as_deref(),
            Some("Game (3)")
        );
        assert_eq!(keep_both_stem("Game", |_| true), None);
    }

    #[test]
    fn a_partial_counts_as_in_use() {
        let dir = scratch("inuse");
        let target = dir.join("Game");
        assert!(!name_in_use(&target));
        std::fs::create_dir(partial_of(&target)).unwrap();
        assert!(name_in_use(&target));
        std::fs::write(dir.join("Other.pkg"), b"x").unwrap();
        assert!(name_in_use(&dir.join("Other.pkg")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn game(root: &Path) -> PathBuf {
        let game = root.join("PPSA99999-app");
        std::fs::create_dir_all(game.join("sce_sys")).unwrap();
        std::fs::create_dir_all(game.join("data/sub")).unwrap();
        std::fs::create_dir_all(game.join("empty/deeper")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(game.join("eboot.bin"), &big).unwrap();
        std::fs::write(
            game.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99999\"}",
        )
        .unwrap();
        std::fs::write(game.join("data/sub/a.bin"), b"").unwrap();
        std::fs::write(game.join("data/zeros.bin"), vec![0u8; 200_000]).unwrap();
        game
    }

    fn scratch(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ps5upload-fold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Every file and empty folder under `dir`, with contents.
    fn snapshot(dir: &Path) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
        let (files, empty) = ps5upload_fpkg::source::scan_tree(dir).unwrap();
        let mut out: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|f| (f.path.clone(), std::fs::read(dir.join(&f.path)).unwrap()))
            .collect();
        out.sort();
        let mut empty: Vec<String> = empty
            .iter()
            .map(|d| d.trim_matches('/').to_string())
            .collect();
        empty.sort();
        (out, empty)
    }

    fn unpack(image: &Path, target: &Path, mode: VerifyMode) -> Result<FolderBuilt, String> {
        let mut tree = ps5upload_fpkg::source::open(image).map_err(|e| e.to_string())?;
        let path = image.to_path_buf();
        let reopen = move || ps5upload_fpkg::source::open(&path).map_err(|e| e.to_string());
        let cancel = AtomicBool::new(false);
        let mut stages: Vec<String> = Vec::new();
        let r = build(
            &mut *tree,
            &reopen,
            target,
            mode,
            &cancel,
            &mut |s, _, _| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            },
        );
        if r.is_ok() {
            assert_eq!(stages, ["write", "verify"]);
        }
        r
    }

    /// A PS4 fixture package of ps5upload-fpkg (tests/fixtures/ps4), written to `dir`.
    fn ps4_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
        let gz: &[u8] = match name {
            "synthetic" => {
                include_bytes!("../../ps5upload-fpkg/tests/fixtures/ps4/synthetic.pkg.gz")
            }
            "update" => include_bytes!("../../ps5upload-fpkg/tests/fixtures/ps4/update.pkg.gz"),
            "addon" => include_bytes!("../../ps5upload-fpkg/tests/fixtures/ps4/addon.pkg.gz"),
            _ => panic!("no fixture {name}"),
        };
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut bytes).unwrap();
        let path = dir.join(format!("{name}.pkg"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A PS4 game with its update merged in becomes one folder at the update's version, with
    /// the param.json ShadowMount+ reads, and reads back against the merge opened again; an
    /// add-on becomes a folder of its files alone.
    #[test]
    fn a_ps4_update_merges_into_its_game_folder_and_an_addon_unpacks() {
        let root = scratch("ps4merge");
        let (game, update, addon) = (
            ps4_fixture(&root, "synthetic"),
            ps4_fixture(&root, "update"),
            ps4_fixture(&root, "addon"),
        );
        let target = root.join("merged");
        let (mut tree, _, _) =
            ps5upload_fpkg::module_case::open_with_update(&game, Some(&update)).unwrap();
        let (g, u) = (game.clone(), update.clone());
        let reopen = move || {
            ps5upload_fpkg::module_case::open_with_update(&g, Some(&u))
                .map(|(t, _, _)| t)
                .map_err(|e| e.to_string())
        };
        let cancel = AtomicBool::new(false);
        build(
            &mut *tree,
            &reopen,
            &target,
            VerifyMode::Full,
            &cancel,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            std::fs::read(target.join("eboot.bin")).unwrap(),
            b"\x7fELF patched eboot 01.05"
        );
        assert_eq!(
            std::fs::read(target.join("data/added.bin")).unwrap(),
            b"added by the update"
        );
        assert!(
            target.join("data/empty.bin").is_file(),
            "the game's own file"
        );
        let sfo = std::fs::read(target.join("sce_sys/param.sfo")).unwrap();
        let params = ps5upload_pkg::sfo_params(&sfo).unwrap();
        let text = |k: &str| ps5upload_fpkg::ps4_pkg::sfo_text(&params, k).unwrap();
        assert_eq!(
            (text("CATEGORY"), text("APP_VER")),
            ("gd".into(), "01.05".into())
        );
        let json = std::fs::read_to_string(target.join("sce_sys/param.json")).unwrap();
        assert!(json.contains("CUSA99990"), "{json}");

        let target = root.join("addon");
        let mut tree = ps5upload_fpkg::source::open(&addon).unwrap();
        let a = addon.clone();
        let reopen = move || ps5upload_fpkg::source::open(&a).map_err(|e| e.to_string());
        build(
            &mut *tree,
            &reopen,
            &target,
            VerifyMode::Full,
            &cancel,
            &mut |_, _, _| {},
        )
        .unwrap();
        let (files, _) = snapshot(&target);
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            ["dlc/map.bin", "dlc/readme.txt"],
            "no sce_sys, no param.json"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A libSceAmpr game's stale index doesn't reach the folder, and the read-back sees the same
    /// fresh one; an LZ4-packed game's index is kept, since it also lists what is in the packs.
    #[test]
    fn a_stale_ampr_index_is_replaced_unless_the_game_is_packed() {
        let root = scratch("ampr");
        let src = root.join("PPSA77778");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        let mut eboot = vec![7u8; 5000];
        eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(src.join("eboot.bin"), &eboot).unwrap();
        std::fs::write(src.join("sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(src.join("ampr_emu.index"), b"stale").unwrap();
        let run = |name: &str| {
            let target = root.join(name);
            let mut tree = FolderSource::open(&src).unwrap();
            let again = src.clone();
            let reopen = move || -> Result<Box<dyn SourceTree>, String> {
                Ok(Box::new(
                    FolderSource::open(&again).map_err(|e| e.to_string())?,
                ))
            };
            build(
                &mut tree,
                &reopen,
                &target,
                VerifyMode::Full,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap();
            std::fs::read(target.join("ampr_emu.index")).unwrap()
        };
        let fresh = ps5upload_fpkg::ampr_index::parse(&run("fresh")).expect("a real index");
        assert!(fresh
            .iter()
            .any(|e| e.path == "eboot.bin" && e.size == 5000));
        std::fs::write(src.join("ampr_assets.index"), b"AMPRPAK4 and the rest").unwrap();
        assert_eq!(run("packed"), b"stale");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An unsigned eboot in a game unpacked to a folder comes out fake-signed, and the folder
    /// verifies: the verify stage reopens the source and must see the same bytes the write did.
    #[test]
    fn an_unsigned_eboot_is_fake_signed_and_the_folder_verifies() {
        let root = scratch("unsigned");
        let src = root.join("PPSA77776");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        let mut elf: Vec<u8> = (0..0x3000u32).map(|i| (i * 7 + 3) as u8).collect();
        elf[..0x78].fill(0);
        elf[..8].copy_from_slice(&[0x7F, b'E', b'L', b'F', 2, 1, 1, 9]);
        elf[0x10..0x12].copy_from_slice(&0xFE10u16.to_le_bytes());
        elf[0x12..0x14].copy_from_slice(&0x3Eu16.to_le_bytes());
        elf[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
        elf[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes());
        elf[0x34..0x36].copy_from_slice(&0x40u16.to_le_bytes());
        elf[0x36..0x38].copy_from_slice(&0x38u16.to_le_bytes());
        elf[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes());
        elf[0x40..0x44].copy_from_slice(&1u32.to_le_bytes());
        elf[0x48..0x50].copy_from_slice(&0x1000u64.to_le_bytes());
        elf[0x60..0x68].copy_from_slice(&0x2000u64.to_le_bytes());
        std::fs::write(src.join("eboot.bin"), &elf).unwrap();
        std::fs::write(src.join("sce_sys/param.json"), b"{}").unwrap();

        let (mut tree, _, signing) = ps5upload_fpkg::module_case::open(&src).unwrap();
        assert_eq!(signing.signed, ["eboot.bin"]);
        let path = src.clone();
        let reopen = move || {
            ps5upload_fpkg::module_case::open(&path)
                .map(|(t, _, _)| t)
                .map_err(|e| e.to_string())
        };
        let target = root.join("out");
        build(
            &mut *tree,
            &reopen,
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let out = std::fs::read(target.join("eboot.bin")).unwrap();
        assert_eq!(out[..4], [0x4F, 0x15, 0x3D, 0x1D]);
        assert!(out.windows(0x2000).any(|w| w == &elf[0x1000..0x3000]));
        assert_eq!(std::fs::read(src.join("eboot.bin")).unwrap(), elf);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An image whose module the eboot loads in another letter case unpacks with the eboot's
    /// spelling, and verifies: the verify stage reopens the source, and must see the same names
    /// the write did (it failed with "not in this exFAT volume" when it did not).
    #[test]
    fn a_module_takes_the_eboot_spelling_and_the_folder_verifies() {
        let root = scratch("case");
        let src = root.join("PPSA77777");
        std::fs::create_dir_all(src.join("Media/Modules")).unwrap();
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        let mut eboot = vec![7u8; 5000];
        eboot.extend_from_slice(b"\0%s/Modules/Il2CppUserAssemblies.prx\0");
        std::fs::write(src.join("eboot.bin"), &eboot).unwrap();
        std::fs::write(src.join("sce_sys/param.json"), b"{}").unwrap();
        let module: Vec<u8> = (0..70_000u32).map(|i| (i % 13) as u8).collect();
        std::fs::write(src.join("Media/Modules/Il2cppUserAssemblies.prx"), &module).unwrap();
        let image = root.join("PPSA77777.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        image_build::build(
            ImageFormat::Exfat,
            None,
            &mut tree,
            &image,
            VerifyMode::Full,
            8 << 20,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let target = root.join("out");
        let (mut tree, renamed, _) = ps5upload_fpkg::module_case::open(&image).unwrap();
        assert_eq!(renamed.len(), 1);
        let path = image.clone();
        let reopen = move || {
            ps5upload_fpkg::module_case::open(&path)
                .map(|(t, _, _)| t)
                .map_err(|e| e.to_string())
        };
        build(
            &mut *tree,
            &reopen,
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let names: Vec<String> = std::fs::read_dir(target.join("Media/Modules"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Il2CppUserAssemblies.prx"]);
        assert_eq!(
            std::fs::read(target.join("Media/Modules/Il2CppUserAssemblies.prx")).unwrap(),
            module
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each image Convert writes (exFAT, UFS2, PFS, and a compressed `.ffpfsc`) unpacks into a
    /// folder equal to the one it was built from, empty folders included.
    #[test]
    fn every_image_unpacks_to_the_folder_it_came_from() {
        let root = scratch("rt");
        let src = game(&root);
        let want = snapshot(&src);
        let cases = [
            (ImageFormat::Exfat, false),
            (ImageFormat::Ffpkg, false),
            (ImageFormat::Ffpfs, false),
            (ImageFormat::Exfat, true),
        ];
        for (n, (format, compress)) in cases.into_iter().enumerate() {
            let (name, inner) = image_build::output_names(&src, format, compress);
            let image = root.join(&name);
            let mut tree = FolderSource::open(&src).unwrap();
            image_build::build(
                format,
                inner.as_deref(),
                &mut tree,
                &image,
                VerifyMode::Full,
                8 << 20,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap();
            for mode in [
                VerifyMode::Full,
                VerifyMode::Fast {
                    seed: 7,
                    seam: false,
                },
            ] {
                let target = root.join(format!("out-{n}-{}", matches!(mode, VerifyMode::Full)));
                let built = unpack(&image, &target, mode).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(built.files, 4, "{name}");
                assert_eq!(snapshot(&target), want, "{name}");
                assert!(!partial_of(&target).exists());
                assert!(!target.join(MARKER).exists());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_folder_is_refused_and_left_alone() {
        let root = scratch("ex");
        let src = game(&root);
        let target = root.join("taken");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("mine.txt"), b"keep").unwrap();
        let err = unpack(&src, &target, VerifyMode::Full).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(target.join("mine.txt")).unwrap(), b"keep");
        assert!(!partial_of(&target).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cancel_leaves_no_partial_folder() {
        let root = scratch("cx");
        let src = game(&root);
        let target = root.join("out");
        let mut tree = FolderSource::open(&src).unwrap();
        let reopen = || -> Result<Box<dyn SourceTree>, String> { unreachable!() };
        let cancel = AtomicBool::new(false);
        let err = build(
            &mut tree,
            &reopen,
            &target,
            VerifyMode::Full,
            &cancel,
            &mut |_, done, _| {
                if done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!target.exists() && !partial_of(&target).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A source tree with whatever paths a test wants, every file a few bytes long.
    /// One file of `size` bytes, each byte its offset's low bits.
    struct OneBigFile(Vec<SourceFile>, u64);

    fn big_file_bytes(size: u64) -> Vec<u8> {
        (0..size).map(|i| (i % 251) as u8).collect()
    }

    impl SourceTree for OneBigFile {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn read(&mut self, _: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            Ok(big_file_bytes(self.1))
        }
        fn describe(&self) -> String {
            "one big file".into()
        }
    }

    /// #669: the read-back of a big file moves the bar as it reads, not only when the file
    /// ends, and always goes forward, up to exactly the total.
    #[test]
    fn the_read_back_moves_through_a_big_file() {
        let root = scratch("verify-moves");
        std::fs::create_dir_all(&root).unwrap();
        let size = CHUNK * 3 + 7;
        std::fs::write(root.join("big.bin"), big_file_bytes(size)).unwrap();
        let mut source = OneBigFile(
            vec![SourceFile {
                path: "big.bin".into(),
                size,
            }],
            size,
        );
        let mut seen: Vec<(u64, u64)> = Vec::new();
        verify(
            &root,
            &[("big.bin".to_string(), size)],
            &[],
            &mut source,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |d, t| seen.push((d, t)),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(seen.iter().all(|&(_, t)| t == size));
        let dones: Vec<u64> = seen.iter().map(|&(d, _)| d).collect();
        assert!(dones.windows(2).all(|w| w[0] <= w[1]), "{dones:?}");
        assert_eq!(dones.first(), Some(&0));
        assert_eq!(dones.last(), Some(&size));
        // Four chunks from each side, then the final report for the piece.
        let between = dones.iter().filter(|&&d| d > 0 && d < size).count();
        assert!(between >= 6, "{dones:?}");
    }

    #[test]
    fn each_side_of_a_piece_moves_the_bar_half_as_far() {
        let mut seen = Vec::new();
        let mut report = |d: u64, t: u64| seen.push((d, t));
        let mut sides = BothSides::new(100, 1000, &mut report);
        sides.read(50);
        sides.read(50);
        sides.read(100);
        assert_eq!(seen, [(125, 1000), (150, 1000), (200, 1000)]);
    }

    struct Listed(Vec<SourceFile>, Vec<String>);

    impl SourceTree for Listed {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn empty_dirs(&self) -> &[String] {
            &self.1
        }
        fn read(&mut self, _: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            Ok(b"abc".to_vec())
        }
        fn describe(&self) -> String {
            "listed".into()
        }
    }

    fn listed(paths: &[&str]) -> Listed {
        Listed(
            paths
                .iter()
                .map(|p| SourceFile {
                    path: p.to_string(),
                    size: 3,
                })
                .collect(),
            Vec::new(),
        )
    }

    /// A path that would land outside the folder, or a name the drive cannot hold, fails the
    /// build naming the file, before anything is written (not even the `.partial`).
    #[test]
    fn bad_names_fail_before_anything_is_written() {
        let root = scratch("bad");
        let target = root.join("out");
        let reopen = || -> Result<Box<dyn SourceTree>, String> { unreachable!() };
        let mut bad = vec![
            "ok.bin|../evil.bin",
            "ok.bin|/abs.bin",
            "ok.bin|a//b.bin",
            "a\0b",
        ];
        if cfg!(windows) {
            bad.extend(["ok.bin|CON.txt", "ok.bin|what?.bin", "ok.bin|trail."]);
        }
        for case in bad {
            let paths: Vec<&str> = case.split('|').collect();
            let mut tree = listed(&paths);
            let err = build(
                &mut tree,
                &reopen,
                &target,
                VerifyMode::Full,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap_err();
            let named = paths.last().unwrap();
            assert!(err.starts_with(named), "{case}: {err}");
            assert!(!target.exists() && !partial_of(&target).exists(), "{case}");
        }
        // A file that is also a folder, and the marker's own name.
        for paths in [&["a", "a/b"][..], &[MARKER][..]] {
            let mut tree = listed(paths);
            assert!(build(
                &mut tree,
                &reopen,
                &target,
                VerifyMode::Full,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .is_err());
            assert!(!partial_of(&target).exists());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_names_are_checked_on_every_platform() {
        assert!(path_problem("sce_sys/param.json", true).is_none());
        assert!(path_problem("a/CON", true).is_some());
        assert!(path_problem("lpt1.txt", true).is_some());
        assert!(path_problem("COM0", true).is_none());
        assert!(path_problem("CONSOLE.txt", true).is_none());
        assert!(path_problem("C:/x", true).is_some());
        assert!(path_problem("a:b", true).is_some());
        assert!(path_problem("a:b", false).is_none());
        assert!(path_problem("dir /x", true).is_some());
        assert!(path_problem("..", false).is_some());
        assert!(path_problem("a/./b", false).is_some());
        assert!(name_problem("Game").is_none());
        assert!(name_problem("a/b").is_some());
        assert!(name_problem("..").is_some());
        assert!(name_problem("x.partial").is_some());
    }

    #[test]
    fn only_a_marked_partial_folder_is_ours_and_swept() {
        let root = scratch("own");
        let ours = root.join("A.partial");
        let theirs = root.join("B.partial");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(ours.join(MARKER), b"").unwrap();
        assert!(is_our_partial_dir(&ours));
        assert!(!is_our_partial_dir(&theirs));
        // The output folder's sweep removes ours, unless a running build is writing it.
        let active = root.join("C.partial");
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(active.join(MARKER), b"").unwrap();
        let writing = crate::fpkg_api::Writing::claim(active.clone()).unwrap();
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(active.join(MARKER), b"").unwrap();
        crate::fpkg_api::sweep_output_leftovers(&root);
        assert!(!ours.exists());
        assert!(theirs.is_dir() && active.is_dir());
        // A second build of the same name is refused while the first runs.
        assert!(crate::fpkg_api::Writing::claim(active.clone()).is_none());
        drop(writing);
        crate::fpkg_api::sweep_output_leftovers(&root);
        assert!(!active.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
