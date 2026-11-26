//! Console → computer downloads over AVA1: a folder or file landing on disk
//! (`to_local`) and a folder or file streamed straight into a `.zip` (`to_zip`).
//! Blocking, like the upload adapters: call from `spawn_blocking` or a plain thread.
//!
//! The console check (`console::require_ava1`), the terminal-versus-retryable split, the
//! `error_reason` words and the retry/backoff loop are the upload adapters' own
//! (`upload.rs`): a download differs only in which side holds the sink.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::gen;
use anyhow::{anyhow, Result};
use ava1::journal::{self, State};
use ava1::manifest::{self, Manifest};
use ava1::packlog::LoggedGroup;
use ava1::recv::{download_job, is_fd_exhausted, LocalSink, RecvOptions, Sink};
use ava1::send::{Progress, SendError};
use ps5upload_core::download::DownloadKind;
// Shared with the archive unpacker, so archive-supplied names follow the same rule.
use ps5upload_core::transfer::host_unsafe_component;
use zip::write::SimpleFileOptions;

use crate::pool::{pool, Pool};
use crate::upload::{rearm, refusal, wait, SessionGate, UploadFailure, STALL_LIMIT};
use crate::zip_stored::StoredZipSink;

/// The grant a download extends to the console (SPEC.md §12.4).
const CREDIT: u64 = 64 << 20;

/// The counters the engine's 200 ms ticker reads. All are stored absolutely (never
/// `fetch_add`), and only ever raised: a counter that steps backwards reads as a bug
/// to the person watching it. `total` is the ticker's dynamic total
/// (`TickerContext::dynamic_total_bytes`): the AVA1 path only learns the size when the
/// console's manifest arrives, so it is filled from the manifest, not from a Status.
#[derive(Clone, Default)]
pub struct Counters {
    pub bytes: Arc<AtomicU64>,
    pub files: Arc<AtomicU64>,
    pub files_finalized: Arc<AtomicU64>,
    pub bytes_finalized: Arc<AtomicU64>,
    pub total: Option<Arc<AtomicU64>>,
}

/// Copies AVA1's progress into the counters every 200 ms until dropped, and once more on
/// drop. Mapping (the upload bridge's rule): AVA1 only knows a file is done when it is
/// durable, so `bytes` and `bytes_finalized` ← `bytes_durable`, `files` and
/// `files_finalized` ← `files_durable`. `base_*` carry the work of earlier attempts
/// (zip only): the counter is monotonic "work done", capped at the archive's own size
/// so a restart never shows more than 100%.
struct Ticker {
    handle: tokio::task::AbortHandle,
    state: Arc<TickState>,
}

struct TickState {
    p: Arc<Progress>,
    c: Counters,
    base_bytes: u64,
    base_files: u64,
}

impl TickState {
    fn store(&self) {
        let mut bytes = self.base_bytes + self.p.bytes_durable.load(Ordering::Relaxed);
        let mut files = self.base_files + self.p.files_durable.load(Ordering::Relaxed);
        // What is shown never passes the whole (review 019 F3): after a zip restart the work
        // of the earlier attempts is added on top, and "work done" may exceed the archive.
        // Capped here, the bar stops at 100% and the ETA at zero instead of going past it.
        let (total_bytes, total_files) = (
            self.p.bytes_total.load(Ordering::Relaxed),
            self.p.files_total.load(Ordering::Relaxed),
        );
        if total_bytes > 0 {
            bytes = bytes.min(total_bytes);
        }
        if total_files > 0 {
            files = files.min(total_files);
        }
        self.c.bytes.fetch_max(bytes, Ordering::Relaxed);
        self.c.bytes_finalized.fetch_max(bytes, Ordering::Relaxed);
        self.c.files.fetch_max(files, Ordering::Relaxed);
        self.c.files_finalized.fetch_max(files, Ordering::Relaxed);
        if let Some(t) = &self.c.total {
            let total = self.p.bytes_total.load(Ordering::Relaxed);
            if total > 0 {
                t.store(total, Ordering::Release);
            }
        }
    }
}

impl Ticker {
    fn start(p: Arc<Progress>, c: &Counters, base_bytes: u64, base_files: u64) -> Ticker {
        let state = Arc::new(TickState {
            p,
            c: c.clone(),
            base_bytes,
            base_files,
        });
        let s = state.clone();
        let handle = tokio::spawn(async move {
            loop {
                s.store();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .abort_handle();
        Ticker { handle, state }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.handle.abort();
        self.state.store();
    }
}

/// The name `src` lands under on this computer. With `escape_names` (see
/// [`to_local_with_in`]) a name this computer can't hold is kept under its reversible form
/// instead of refused.
fn basename_kept(src: &str, escape_names: bool) -> Result<String> {
    if !escape_names {
        return basename(src).map(str::to_owned);
    }
    let name = src.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        return Err(anyhow!("{src:?} has no usable file name"));
    }
    ps5upload_core::host_names::escape_name(name).map_err(|why| {
        UploadFailure {
            reason: "ava1_bad_manifest".into(),
            detail: why,
        }
        .into()
    })
}

fn basename(src: &str) -> Result<&str> {
    let name = src.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." || name.contains('\\') {
        return Err(anyhow!("{src:?} has no usable file name"));
    }
    // Written on THIS computer, whatever it is: the same rule the entries inside get from
    // `check_shape`. On Windows `CON` is a device, `a:b` a stream and `x.` another name.
    if let Some(why) = host_unsafe_component(name) {
        return Err(UploadFailure {
            reason: "ava1_bad_manifest".into(),
            detail: format!("{name:?} cannot be saved on this computer: {why}"),
        }
        .into());
    }
    Ok(name)
}

/// What the manifest of a download may look like (peer-supplied data: `Manifest::
/// from_pages` already ran `check_path` on every path; this adds the shape the request
/// promised and the one path rule `check_path` leaves to the host OS).
pub(crate) fn check_shape(m: &Manifest, single: bool) -> io::Result<()> {
    check_shape_with(m, single, false)
}

/// `check_shape`; with `escape_names` a name this computer can't hold is fine, since it is
/// written under its reversible form (`kept_manifest`), which can neither climb nor misname.
fn check_shape_with(m: &Manifest, single: bool, escape_names: bool) -> io::Result<()> {
    let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    for e in &m.entries {
        kept_path(&e.path, escape_names)?;
    }
    if single {
        let files = m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .count();
        if m.entries.len() != 1 || files != 1 {
            return Err(bad(format!(
                "a single-file download received a manifest of {} entries",
                m.entries.len()
            )));
        }
    }
    Ok(())
}

/// The root-relative path a manifest `path` is written under on this computer, after the
/// checks every peer-supplied path passes before anything is written (or removed) under it:
/// itself, or with `escape_names` its reversible form. Err for a path that could climb out of
/// the root or name something else on some host.
fn kept_path(path: &str, escape_names: bool) -> io::Result<String> {
    let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    manifest::check_path(path).map_err(|e| bad(e.to_string()))?;
    if escape_names {
        // Component by component: a '\' is part of a name here, never a separator.
        let kept = ps5upload_core::host_names::escape_path(path)
            .map_err(|why| bad(format!("{path:?}: {why}")))?;
        if kept
            .split('/')
            .any(|c| c.contains('\\') || host_unsafe_component(c).is_some())
        {
            return Err(bad(format!("{path:?} cannot be kept on this computer")));
        }
        return Ok(kept);
    }
    // A backslash is a separator on Windows: `a\..\b` would climb out of the root.
    if path.contains('\\') {
        return Err(bad(format!("{path:?} contains a backslash")));
    }
    // Peer-supplied names are written on THIS computer, whatever it is. A ':' makes
    // `C:/x` a drive path and `C:evil` a drive-relative one on Windows (and an
    // alternate data stream on NTFS); the reserved device names and a trailing dot
    // or space name something other than the file asked for. Refused everywhere so
    // the console's data lands the same way on every host.
    for comp in path.split('/') {
        if let Some(why) = host_unsafe_component(comp) {
            return Err(bad(format!("{path:?}: {why}")));
        }
    }
    Ok(path.to_owned())
}

/// `LocalSink` plus the checks above. The landing root is `dest_dir/<basename>` for both
/// kinds (the manifest's paths are root-relative), and every byte goes through a
/// `.ava-part` sibling in the destination's own directory, renamed once at the end.
struct CheckedSink {
    inner: LocalSink,
    root: PathBuf,
    single: bool,
    /// Names this computer can't hold are written under their reversible form.
    escape_names: bool,
    /// The manifest as landed (kept names), for the files' times at `finish`.
    landed: Mutex<Option<Manifest>>,
}

/// The PS5's modification times on the files just landed (#812): without them every
/// download is dated when it arrived. Best effort: a time that cannot be set leaves the
/// file as it is (the bytes are what matters).
fn stamp_mtimes(root: &Path, single: bool, m: &Manifest) {
    for e in m
        .entries
        .iter()
        .filter(|e| e.kind == gen::ENTRY_FILE && e.mtime > 0)
    {
        let p = if single {
            root.to_path_buf()
        } else {
            root.join(&e.path)
        };
        let Some(t) = std::time::UNIX_EPOCH.checked_add(Duration::from_secs(e.mtime)) else {
            continue;
        };
        let _ = set_mtime(&p, t);
    }
}

fn set_mtime(p: &Path, t: std::time::SystemTime) -> io::Result<()> {
    // Only a regular file the download landed, never the target of a link.
    if !std::fs::symlink_metadata(p)?.is_file() {
        return Ok(());
    }
    let f = std::fs::OpenOptions::new().write(true).open(p)?;
    f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
}

/// Two paths of `m` that differ only in case, one of them a file (the console's own disk
/// tells `Save.dat` from `save.dat`).
fn case_twins(m: &Manifest) -> Option<(&str, &str)> {
    let mut seen: std::collections::HashMap<String, &manifest::Entry> =
        std::collections::HashMap::new();
    for e in &m.entries {
        if let Some(first) = seen.insert(e.path.to_lowercase(), e) {
            if first.path != e.path && (first.kind == gen::ENTRY_FILE || e.kind == gen::ENTRY_FILE)
            {
                return Some((&first.path, &e.path));
            }
        }
    }
    None
}

/// Whether the folder `root` is (or will be) made in ignores case in names, asked of the
/// nearest folder that exists (`root` itself, or the one it will be made in): the disk that
/// folder is on is the one the files land on. Never of the folder above it: at a disk's root
/// (`/Volumes/USB`, `D:\`) that is another disk, or none, and its answer said nothing about
/// this one.
fn ignores_case(root: &Path) -> bool {
    match root.ancestors().find(|p| p.is_dir()) {
        Some(dir) => folder_ignores_case(dir),
        None => false,
    }
}

/// `name` with its ASCII letters in the other case.
fn other_case(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Whether `dir` finds its own entries under their names in the other case. A name with no
/// letter says nothing, and a folder holding both spellings of one tells them apart; a folder
/// with no name to ask is asked with a probe file of its own, removed at once.
fn folder_ignores_case(dir: &Path) -> bool {
    let Ok(it) = std::fs::read_dir(dir) else {
        return false;
    };
    let names: std::collections::HashSet<std::ffi::OsString> =
        it.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
    for name in names.iter().filter_map(|n| n.to_str()) {
        let other = other_case(name);
        if other == name {
            continue;
        }
        if names.contains(std::ffi::OsStr::new(&other)) {
            return false;
        }
        return std::fs::symlink_metadata(dir.join(other)).is_ok();
    }
    static PROBES: AtomicU64 = AtomicU64::new(0);
    let probe = dir.join(format!(
        ".ps5upload-case-probe-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, Ordering::Relaxed)
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(f) => {
            drop(f);
            let name = probe.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let folded = std::fs::symlink_metadata(dir.join(other_case(name))).is_ok();
            let _ = std::fs::remove_file(&probe);
            folded
        }
        Err(_) => false,
    }
}

/// Err when `m` holds two names that are one name in `root`: both were written to the one
/// file, the later over the earlier, and the download ended as done with a file's bytes gone.
fn refuse_case_twins(m: &Manifest, root: &Path) -> io::Result<()> {
    match case_twins(m) {
        Some((a, b)) if ignores_case(root) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{a:?} and {b:?} differ only in capital letters, and {} keeps one file for both; download to a disk that tells them apart",
                root.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// The in-place part file of the file at `rel` (root-relative), as a merge writes it.
fn part_name(rel: &str) -> String {
    format!("{rel}.ava-part")
}

/// A file of `m` and another entry of `m` at that file's in-place part file (`X` and
/// `X.ava-part`), compared as `fold` (lowercased) or exactly.
fn part_name_clash(m: &Manifest, fold: bool) -> Option<(&str, &str)> {
    let key = |p: &str| if fold { p.to_lowercase() } else { p.to_owned() };
    let paths: std::collections::HashMap<String, &str> = m
        .entries
        .iter()
        .map(|e| (key(&e.path), e.path.as_str()))
        .collect();
    m.entries
        .iter()
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .find_map(|e| {
            paths
                .get(&key(&part_name(&e.path)))
                .map(|other| (e.path.as_str(), *other))
        })
}

/// Err when `m`, downloaded into the folder `root` that already exists, holds both `X` and
/// `X.ava-part`. Such a download writes each large file as `<name>.ava-part` beside its place
/// and renames it at the end, so the one path was two files at once: `X` ended with the other
/// file's bytes, or `X.ava-part` was renamed away as `X`. A folder that does not exist yet is
/// staged whole and writes nothing under a part name, so the same tree downloads fine there.
fn refuse_part_name_clash(m: &Manifest, root: &Path) -> io::Result<()> {
    let Some((a, b)) = part_name_clash(m, true) else {
        return Ok(());
    };
    if part_name_clash(m, false).is_none() && !ignores_case(root) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{b:?} is the name this app gives {a:?} while it is written into a folder that already exists; download to a folder that does not have {} yet",
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string())
        ),
    ))
}

/// `m` with every path in the form it is kept under on this computer
/// (`host_names::escape_path`). The entries keep their order, so their file ids.
fn kept_manifest(m: &Manifest) -> io::Result<Manifest> {
    let mut kept = m.clone();
    for e in &mut kept.entries {
        e.path = ps5upload_core::host_names::escape_path(&e.path)
            .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))?;
    }
    Ok(kept)
}

impl Sink for CheckedSink {
    /// The checks that need nothing from the disk, run by the library before the job's journal
    /// or manifest is written (ava1 1.0.13): a manifest refused here leaves no trace. `prepare`
    /// runs them again with the disk checks.
    fn check(&self, m: &Manifest) -> io::Result<()> {
        check_shape_with(m, self.single, self.escape_names)?;
        if self.escape_names {
            self.inner.check(&kept_manifest(m)?)
        } else {
            self.inner.check(m)
        }
    }

    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        check_shape_with(m, self.single, self.escape_names)?;
        let kept;
        let m = if self.escape_names {
            kept = kept_manifest(m)?;
            &kept
        } else {
            m
        };
        if !self.single {
            refuse_case_twins(m, &self.root)?;
        }
        // Into a folder that already exists (not staged), large files go through in-place
        // part files: the decision as the `LocalSink` holds it, a resume's included.
        if !self.single && self.inner.resume_key().is_some_and(|(_, staged)| !staged) {
            refuse_part_name_clash(m, &self.root)?;
        }
        *self.landed.lock().unwrap() = Some(m.clone());
        if self.single && self.root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} is a folder; a file cannot replace it",
                    self.root.display()
                ),
            ));
        }
        self.inner.prepare(m)
    }
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.inner.write_at(id, off, data)
    }
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.inner.write_whole(id, data)
    }
    fn write_whole_root(&self, id: u32, root: &[u8; 32], data: &[u8]) -> io::Result<()> {
        self.inner.write_whole_root(id, root, data)
    }
    fn sync(&self, ids: &[u32]) -> io::Result<()> {
        self.inner.sync(ids)
    }
    // The durable-by-log hooks go to the `LocalSink` too (the log is its decision: on by
    // default off macOS, `AVA1_LOG_SMALL` to override), or a download would never
    // take that path while the receiver believed it could.
    fn enable_log(&self, dir: &Path) {
        self.inner.enable_log(dir)
    }
    fn sync_batch(&self, small: &[u32], large: &[u32]) -> io::Result<Vec<LoggedGroup>> {
        self.inner.sync_batch(small, large)
    }
    fn batch_journaled(&self, groups: &[LoggedGroup]) {
        self.inner.batch_journaled(groups)
    }
    fn unswept(&self) -> usize {
        self.inner.unswept()
    }
    fn log_pressure(&self) -> bool {
        self.inner.log_pressure()
    }
    fn sweep(&self, force: bool) -> io::Result<Vec<u32>> {
        self.inner.sweep(force)
    }
    fn sweep_journaled(&self, ids: &[u32]) {
        self.inner.sweep_journaled(ids)
    }
    fn recover_log(&self, st: &State) -> io::Result<Vec<u32>> {
        self.inner.recover_log(st)
    }
    fn log_cleanup(&self) {
        self.inner.log_cleanup()
    }
    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read_at(id, off, buf)
    }
    fn commit(&self, id: u32) -> io::Result<()> {
        self.inner.commit(id)
    }
    // Both to the `LocalSink` as well. Left to the trait's no-ops, a committed file's folder was
    // not synced before the journal called it done, and a retry of a folder download that had
    // already been renamed into place (the session lost before its JobDone) stayed "staged":
    // its finish found the folder there and failed as "appeared during the download".
    fn sync_committed(&self, ids: &[u32]) -> io::Result<()> {
        self.inner.sync_committed(ids)
    }
    fn resume_staged(&self, journaled: bool, all_done: bool) {
        self.inner.resume_staged(journaled, all_done)
    }
    fn finish(&self) -> io::Result<()> {
        self.inner.finish()?;
        if let Some(m) = self.landed.lock().unwrap().as_ref() {
            stamp_mtimes(&self.root, self.single, m);
        }
        Ok(())
    }
    fn resume_key(&self) -> Option<(String, bool)> {
        self.inner.resume_key()
    }
}

/// The zip entry's name. A folder's entries sit under `<basename>/`; a single file's
/// entry is exactly `<basename>` (the download listing puts the basename in
/// `rel_path` for a file and `download_to_zip_ex` writes it verbatim, so prefixing here
/// would produce `foo.pkg/foo.pkg`).
pub fn zip_entry_name(single: bool, basename: &str, rel: &str) -> String {
    if single {
        basename.to_owned()
    } else {
        format!("{basename}/{rel}")
    }
}

struct ZipState {
    m: Option<Arc<Manifest>>,
    zip: Option<zip::ZipWriter<BufWriter<std::fs::File>>>,
    /// The entry being written, and how much of it is in the archive.
    current: Option<u32>,
    written: u64,
    /// Highest id started: ordered delivery never goes back.
    last: Option<u32>,
    started: usize,
    finished: bool,
    /// Bytes of single-group files between their write and their commit. The receiver
    /// verifies such a file by reading it back (`read_at`) before it commits; a deflate
    /// stream cannot be read back, so the sink keeps the (at most one group) bytes
    /// itself. Larger files are verified from their outboards and need nothing.
    kept: std::collections::HashMap<u32, Vec<u8>>,
}

/// An ordered download appended into a `.zip`. Deflate entries cannot be seeked into, so
/// the receiver must deliver each file's bytes contiguously and in file order (the
/// ordered flag); anything else is a protocol violation, never garbage in the archive.
/// Writes `<dest>.ava-part` and renames over `dest` in `finish`, so the final path never
/// holds a half-written archive; an abandoned sink removes its part file.
/// Empty directories are not entries (the zip manifest holds files only). Empty
/// files are appended at `finish`: the receiver creates them up front, out of order,
/// and a zip can only have one entry open at a time.
pub struct ZipSink {
    dest: PathBuf,
    part: PathBuf,
    single: bool,
    base: String,
    st: Mutex<ZipState>,
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e)
}

/// A file arrived a second time (the receiver re-requested it after a verify mismatch).
/// A deflate stream cannot rewrite what it already holds, so the archive attempt is
/// abandoned and restarted — like a dropped connection — instead of being reported as
/// a bad manifest.
#[derive(Debug)]
struct ZipRestart(String);

impl std::fmt::Display for ZipRestart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ZipRestart {}

pub(crate) fn zip_restart(why: impl Into<String>) -> io::Error {
    io::Error::other(ZipRestart(why.into()))
}

/// True for the error a sink returns when a file it already wrote is sent again.
#[doc(hidden)]
pub fn is_zip_restart(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<ZipRestart>())
}

/// A file that keeps failing verification must end the job, not restart forever.
const MAX_RETRY_RESTARTS: u32 = 3;
/// Waits for descriptors to free up before a download gives up on them.
const MAX_FD_WAITS: u32 = 12;

pub(crate) fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

impl ZipSink {
    /// A folder download: entries are `<prefix>/<root-relative path>`.
    pub fn new(path: PathBuf, prefix: &str) -> Self {
        Self::build(path, prefix, false)
    }

    /// A single-file download: the one entry is exactly `name`.
    pub fn single(path: PathBuf, name: &str) -> Self {
        Self::build(path, name, true)
    }

    fn build(dest: PathBuf, base: &str, single: bool) -> Self {
        let mut part = dest.clone().into_os_string();
        part.push(".ava-part");
        Self {
            dest,
            part: PathBuf::from(part),
            single,
            base: base.to_owned(),
            st: Mutex::new(ZipState {
                m: None,
                zip: None,
                current: None,
                written: 0,
                last: None,
                started: 0,
                finished: false,
                kept: Default::default(),
            }),
        }
    }

    /// `mtime`: the file's time on the PS5 (unix seconds, 0 unknown), kept in the entry (#812).
    fn opts(mtime: u64) -> SimpleFileOptions {
        // zip64 so a single >4 GiB game file is encoded correctly (the zip format's rule).
        let o = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        let (time, date) = crate::zip_stored::dos_datetime(mtime);
        match zip::DateTime::try_from_msdos(date, time) {
            Ok(t) if mtime > 0 => o.last_modified_time(t),
            _ => o,
        }
    }

    fn append(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let m =
            st.m.clone()
                .ok_or_else(|| invalid("data before the manifest"))?;
        let e = m
            .entry(id)
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .ok_or_else(|| invalid(format!("data for {id}, which is not a file")))?;
        if e.size == 0 {
            // Written at `finish`; the receiver's up-front empty write lands here.
            return if data.is_empty() {
                Ok(())
            } else {
                Err(invalid(format!("data for empty file {id}")))
            };
        }
        if st.current != Some(id) {
            if st.last.is_some_and(|l| id <= l) {
                // From the start of a file already written: a retry, not a violation.
                return Err(if off == 0 {
                    zip_restart(format!("file {id} was sent again"))
                } else {
                    invalid(format!("file {id} arrived out of order"))
                });
            }
            if let Some(cur) = st.current {
                let want = m.entry(cur).map(|c| c.size).unwrap_or(0);
                if st.written != want {
                    return Err(invalid(format!(
                        "file {cur} ended at {} of {want} bytes",
                        st.written
                    )));
                }
            }
            if off != 0 {
                return Err(invalid(format!(
                    "file {id} starts at offset {off}: a gap in the archive stream"
                )));
            }
            let name = zip_entry_name(self.single, &self.base, &e.path);
            let zip = st.zip.as_mut().ok_or_else(|| invalid("archive not open"))?;
            zip.start_file(name, Self::opts(e.mtime)).map_err(zip_err)?;
            st.current = Some(id);
            st.last = Some(id);
            st.written = 0;
            st.started += 1;
        } else if off == 0 && st.written > 0 {
            // The current file again from its first byte (a retry after a mismatch).
            return Err(zip_restart(format!("file {id} was sent again")));
        } else if off != st.written {
            // A gap or an overlap (a duplicate range): never append it.
            return Err(invalid(format!(
                "file {id} wrote at offset {off} but the archive is at {}",
                st.written
            )));
        }
        if st.written + data.len() as u64 > e.size {
            return Err(invalid(format!("file {id} wrote past its size")));
        }
        let zip = st.zip.as_mut().ok_or_else(|| invalid("archive not open"))?;
        zip.write_all(data)?;
        st.written += data.len() as u64;
        if ava1::verify::groups(e.size) < 2 {
            st.kept.entry(id).or_default().extend_from_slice(data);
        }
        Ok(())
    }
}

impl Drop for ZipSink {
    fn drop(&mut self) {
        let finished = self.st.lock().map(|s| s.finished).unwrap_or(false);
        if !finished {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

impl Sink for ZipSink {
    /// The manifest's shape, checked before the library writes anything of the job.
    fn check(&self, m: &Manifest) -> io::Result<()> {
        check_shape(m, self.single)
    }

    /// Truncates: `File::create` discards whatever an earlier attempt left in the part
    /// file, so two archives' bytes are never mixed.
    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        check_shape(m, self.single)?;
        let f = std::fs::File::create(&self.part)?;
        let mut st = self.st.lock().unwrap();
        st.m = Some(Arc::new(m.clone()));
        st.zip = Some(zip::ZipWriter::new(BufWriter::new(f)));
        st.current = None;
        st.written = 0;
        st.last = None;
        st.started = 0;
        st.kept.clear();
        Ok(())
    }
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.append(id, off, data)
    }
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.append(id, 0, data)
    }
    fn sync(&self, _ids: &[u32]) -> io::Result<()> {
        Ok(())
    }
    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let st = self.st.lock().unwrap();
        let kept = st
            .kept
            .get(&id)
            .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
        let end = off as usize + buf.len();
        if end > kept.len() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        buf.copy_from_slice(&kept[off as usize..end]);
        Ok(buf.len())
    }
    fn commit(&self, id: u32) -> io::Result<()> {
        self.st.lock().unwrap().kept.remove(&id);
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let m =
            st.m.clone()
                .ok_or_else(|| invalid("finished before the manifest"))?;
        let nonempty = m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE && e.size > 0)
            .count();
        if let Some(cur) = st.current {
            let want = m.entry(cur).map(|c| c.size).unwrap_or(0);
            if st.written != want {
                return Err(invalid(format!(
                    "file {cur} ended at {} of {want}",
                    st.written
                )));
            }
        }
        if st.started != nonempty {
            return Err(invalid(format!(
                "the archive holds {} of {nonempty} files",
                st.started
            )));
        }
        let mut zip = st.zip.take().ok_or_else(|| invalid("archive not open"))?;
        for e in m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE && e.size == 0)
        {
            zip.start_file(
                zip_entry_name(self.single, &self.base, &e.path),
                Self::opts(e.mtime),
            )
            .map_err(zip_err)?;
        }
        let mut out = zip.finish().map_err(zip_err)?;
        out.flush()?;
        let f = out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&self.part, &self.dest)?; // same directory by construction
        st.finished = true;
        Ok(())
    }
}

/// Maps the receiver's terminal errors to what the engine's `job_failed_from_err` already
/// understands (`UploadFailure` with a stable `error_reason`). Nothing here is retried:
/// by the time a local write or the final rename fails, the bytes are already down, and
/// asking again would pull gigabytes for the same failure.
fn terminal(e: SendError) -> anyhow::Error {
    match e {
        SendError::Cancelled => anyhow!("transfer_cancelled"),
        // This computer's receiver refused where it would write (AVA1 1.0.7: a symbolic link or
        // a file where a folder of the download goes): this computer's doing, not the PS5's. A
        // path the PS5 refused to send (a protected folder) stays ava1_not_allowed.
        SendError::Refused { status, message }
            if status == gen::ERR_PATH
                && (message.contains("is a symbolic link")
                    || message.contains("is not a folder")) =>
        {
            UploadFailure {
                reason: "ava1_local_path".into(),
                detail: format!("this computer will not save it there: {message}"),
            }
            .into()
        }
        SendError::Refused { status, message } => refusal(status, message).into(),
        SendError::Source(e) if e.kind() == io::ErrorKind::InvalidData => UploadFailure {
            reason: "ava1_bad_manifest".into(),
            detail: format!("the console sent a download this computer will not write: {e}"),
        }
        .into(),
        SendError::Source(e) => UploadFailure {
            reason: "ava1_local_io".into(),
            detail: format!("writing the download on this computer failed: {e}"),
        }
        .into(),
        other => anyhow!(other),
    }
}

/// Job id of attempt `n`: attempt 0 is the job's own id (so the journal directory, the
/// `JobOpen` and the job record agree); a later attempt exists only because the archive
/// was started over, and hashes the id with the attempt so it gets a journal of its own.
/// A resumed connection is the SAME attempt and keeps the id.
fn attempt_id(job_id: [u8; 16], attempt: u32) -> [u8; 16] {
    if attempt == 0 {
        return job_id;
    }
    let mut h = blake3::Hasher::new();
    h.update(&job_id);
    h.update(&attempt.to_le_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

/// One download, retried across connection loss. `make_sink` builds the sink for an
/// attempt.
///
/// `fresh_per_attempt` is the Deflate zip: a deflate stream cannot be seeked into, and the
/// journal's ranges cannot reconstruct compressed bytes, so a dropped connection restarts
/// the archive from byte zero as a NEW job with a new journal (a 40 GiB zip that drops at
/// 90% pays for it again, which is why Stored is the default). The previous attempt's
/// archive is discarded (`ZipSink::prepare` truncates, and an abandoned sink deletes its
/// part file) so two archives are never mixed, and the restart is logged with the attempt
/// number and the bytes spent.
///
/// With `fresh_per_attempt == false` a dropped connection resumes the same job id: the
/// sink re-derives its position from the journal (`Sink::position`; the Stored zip resumes
/// at the durable byte). A file the receiver asks to have re-sent (`ZipRestart`) starts
/// the archive over under either setting, since a zip cannot rewrite what it holds.
#[allow(clippy::too_many_arguments)]
fn run(
    pool: &Pool,
    console: &str,
    src: &str,
    flags: u32,
    job_id: [u8; 16],
    fresh_per_attempt: bool,
    make_sink: &dyn Fn() -> Arc<dyn Sink>,
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    let cancel = cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let jobs_dir = pool.ava_dir().join("jobs");
    let _live = pool.live_job(&job_id); // the journal sweep leaves a running job alone
    ps5upload_core::clog!(
        Debug,
        "transfer",
        "ava1 download {} from {console}:{src}: flags {flags:#x}, fresh per attempt {fresh_per_attempt}",
        crate::upload::hex(&job_id)
    );
    let t0 = Instant::now();
    let outcome = crate::block_on(async {
        SessionGate::identity(pool)?;
        let mut sink = make_sink();
        let mut backoff = Duration::from_millis(250);
        let mut gate = SessionGate::default();
        let (mut base_bytes, mut base_files) = (0u64, 0u64);
        let mut attempt = 0u32;
        let mut retry_restarts = 0u32;
        let mut busy = 0u32;
        let mut fd_waits = 0u32;
        let (mut last_at, mut last_work) = (Instant::now(), 0u64);
        let mut progress = Arc::new(Progress::default());
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("transfer_cancelled"));
            }
            let work = base_bytes + progress.bytes_durable.load(Ordering::Relaxed);
            if work > last_work {
                (last_at, last_work) = (Instant::now(), work);
            } else if last_at.elapsed() > STALL_LIMIT {
                return Err(SessionGate::stalled(
                    pool,
                    console,
                    format!("no durable progress for {STALL_LIMIT:?}; giving up"),
                ));
            }
            let session = match pool.session(console).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate.failed(&e) {
                        return Err(failure.into());
                    }
                    wait(&mut backoff, &e.to_string()).await;
                    continue;
                }
            };
            gate.connected();
            let id = attempt_id(job_id, attempt);
            ps5upload_core::clog!(
                Debug,
                "transfer",
                "ava1 download {}: session up, attempt {attempt}, {base_bytes} bytes already done",
                crate::upload::hex(&job_id)
            );
            let _live_attempt = pool.live_job(&id);
            let mut link = session.job(id);
            let _ticker = Ticker::start(progress.clone(), counters, base_bytes, base_files);
            let o = RecvOptions {
                credit: CREDIT,
                flags,
                jobs_dir: jobs_dir.clone(),
                // `ordered` must agree with the flags (download_job rewrites `flags`).
                ordered: flags & gen::JF_ORDERED != 0,
                progress: progress.clone(),
                cancel: cancel.clone(),
                progress_deadline: None,
            };
            let (started, work_before) = (
                Instant::now(),
                progress.bytes_durable.load(Ordering::Relaxed),
            );
            let (why, dropped) = match download_job(&mut link, src, flags, sink.clone(), o).await {
                Ok(r) => {
                    let _ = std::fs::remove_dir_all(journal::job_dir(&jobs_dir, &id));
                    return Ok(r.bytes);
                }
                Err(SendError::Disconnected(why)) => (why, true),
                // The console answered BUSY to the JobOpen: not now. Same bounded backoff, same attempt.
                Err(SendError::Refused { status, message }) if status == gen::ERR_BUSY => {
                    busy += 1;
                    if busy > pool.busy_tries() {
                        return Err(crate::upload::busy_failure(pool.busy_tries(), &message));
                    }
                    wait(&mut backoff, &format!("the console is busy: {message}")).await;
                    continue;
                }
                // The computer is out of file descriptors (other programs hold them): wait and
                // resume the same attempt; what is durable stays durable. Bounded, and the stall
                // limit ends it too.
                Err(SendError::Source(e)) if is_fd_exhausted(&e) && fd_waits < MAX_FD_WAITS => {
                    fd_waits += 1;
                    (
                        format!("this computer is out of file descriptors: {e}"),
                        true,
                    )
                }
                Err(SendError::Source(e))
                    if is_zip_restart(&e) && retry_restarts < MAX_RETRY_RESTARTS =>
                {
                    retry_restarts += 1;
                    (e.to_string(), false)
                }
                Err(e) => return Err(terminal(e)),
            };
            let durable = progress.bytes_durable.load(Ordering::Relaxed);
            if dropped {
                pool.forget_if(console, &session).await;
                if rearm(&mut backoff, started, work_before, durable) {
                    busy = 0;
                }
            }
            if fresh_per_attempt || !dropped {
                let _ = std::fs::remove_dir_all(journal::job_dir(&jobs_dir, &id));
                base_bytes += durable;
                base_files += progress.files_durable.load(Ordering::Relaxed);
                attempt += 1;
                progress = Arc::new(Progress::default());
                drop(std::mem::replace(&mut sink, make_sink()));
                let _ = writeln!(
                    std::io::stderr(),
                    "ava1: zip download restarts from the beginning (attempt {attempt}, \
                     {base_bytes} bytes already spent): {why}"
                );
                wait(&mut backoff, "restarting the archive").await;
            } else {
                wait(&mut backoff, &format!("{why} ({durable} bytes durable)")).await;
            }
        }
    });
    match &outcome {
        Ok(bytes) => ps5upload_core::clog!(
            Debug,
            "transfer",
            "ava1 download {}: done, {bytes} bytes in {} ms",
            crate::upload::hex(&job_id),
            t0.elapsed().as_millis()
        ),
        Err(e) => ps5upload_core::clog!(
            Debug,
            "transfer",
            "ava1 download {}: ended after {} ms: {e:#}",
            crate::upload::hex(&job_id),
            t0.elapsed().as_millis()
        ),
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
pub fn to_local_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_local_with_in(
        pool,
        console,
        src,
        kind,
        dest_dir,
        unsafe_read,
        false,
        job_id,
        counters,
        cancel,
    )
}

/// `to_local_in`, with `escape_names`: a console name this computer can't hold (`a\b`, `a:b`,
/// `CON`, `x.`) is written under its reversible form (`ps5upload_core::host_names`) instead of
/// refusing the download, and an upload with `TransferConfig::unescape_names` puts it back
/// under the exact name. For the saves code, whose files go back to the console (#729).
#[allow(clippy::too_many_arguments)]
pub fn to_local_with_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    escape_names: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    // The manifest's paths are root-relative, so the landing root is
    // `dest_dir/<basename>` for both kinds; for a file that root is the file's own path.
    // Not `dest_dir` (loses the basename) and not a stripped component (double-nests).
    let target = dest_dir.join(basename_kept(src, escape_names)?);
    // The request kind decides single-file, never the manifest's shape: a folder holding
    // exactly one file must stay a folder.
    let single = kind == DownloadKind::File;
    let mut flags = if single { gen::JF_SINGLE_FILE } else { 0 };
    if unsafe_read {
        flags |= gen::JF_UNSAFE_READ;
    }
    // The staging path every kind of landing uses (`<root>.ava-part`: the staging
    // folder of a new folder, the part file of a single file).
    let part = {
        let mut p = target.clone().into_os_string();
        p.push(".ava-part");
        PathBuf::from(p)
    };
    let job_dir = journal::job_dir(&pool.ava_dir().join("jobs"), &job_id);
    // A fresh job (no journal records) cannot own what is already in the staging path:
    // it is what an earlier, failed download left behind, and renaming it into place
    // would put that manifest's files into this one's folder. A job with a journal is
    // a resume: the staging path is its work.
    if journal_is_empty(&job_dir) {
        remove_path(&part);
    }
    let target_dir = target.clone();
    let sink: Arc<dyn Sink> = Arc::new(CheckedSink {
        inner: LocalSink::new(target.clone(), single),
        root: target,
        single,
        escape_names,
        landed: Mutex::new(None),
    });
    let r = run(
        pool,
        console,
        src,
        flags,
        job_id,
        false,
        &move || sink.clone(),
        counters,
        cancel,
    );
    if let Err(e) = &r {
        cleanup_after_failure(e, &job_dir, &part, &target_dir, single, escape_names);
    }
    r
}

/// What a failed download leaves behind. A cancel, a refusal or a bad manifest is final:
/// nothing will resume the job, so neither the journal nor the staged bytes may outlive it.
/// A local I/O failure (a full disk, a descriptor shortage that outlasted the waits) is
/// recoverable once the cause is gone, and the next run of the same job resumes from the
/// journal and the staged tree, so both stay.
///
/// A folder downloaded into a folder that already exists is not staged: each large file is written
/// as `<name>.ava-part` beside its final place, inside that folder. Those part files belong to the
/// job too; the journal's saved manifest names them (and only them: a `.ava-part` the job does not
/// own is left alone), so they go with the journal (review 009 #5).
fn cleanup_after_failure(
    e: &anyhow::Error,
    job_dir: &Path,
    part: &Path,
    target: &Path,
    single: bool,
    escape_names: bool,
) {
    let recoverable = e
        .downcast_ref::<UploadFailure>()
        .is_some_and(|f| f.reason == "ava1_local_io");
    if !recoverable {
        if !single {
            remove_part_files(job_dir, target, escape_names);
        }
        let _ = std::fs::remove_dir_all(job_dir);
        remove_path(part);
    }
}

/// Removes the in-place `<file>.ava-part` of every file the job's saved manifest lists, under
/// `target` (by their kept names, with `escape_names`). Nothing when the manifest is missing or
/// unreadable (a job that never got that far wrote no part files).
fn remove_part_files(job_dir: &Path, target: &Path, escape_names: bool) {
    let Ok(m) = journal::read_manifest(job_dir) else {
        return;
    };
    for rel in part_files(&m, escape_names) {
        // `remove_file` takes a symlink itself, never what it points at.
        let _ = std::fs::remove_file(target.join(rel));
    }
}

/// The in-place part files (root-relative, by their kept names with `escape_names`) of the
/// files of `m`, but for a part name that is itself a path of `m` in any case: that file is
/// the tree's own (a real `X.ava-part`, landed or about to be replaced), never `X`'s part file.
/// Only paths that pass the writer's own checks (`kept_path`): the journal saves the manifest
/// before `prepare` checks it, so a refused one is read back here, and on Windows `..\x`,
/// `C:x` or `C:\x` joined onto the folder would name a file outside it.
fn part_files(m: &Manifest, escape_names: bool) -> Vec<String> {
    let kept: Vec<(u8, String)> = m
        .entries
        .iter()
        .filter_map(|e| Some((e.kind, kept_path(&e.path, escape_names).ok()?)))
        .collect();
    let paths: std::collections::HashSet<String> =
        kept.iter().map(|(_, p)| p.to_lowercase()).collect();
    kept.iter()
        .filter(|(kind, _)| *kind == gen::ENTRY_FILE)
        .map(|(_, p)| part_name(p))
        .filter(|p| !paths.contains(&p.to_lowercase()))
        .collect()
}

fn journal_is_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => true,
    }
}

fn remove_path(p: &Path) {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            let _ = std::fs::remove_dir_all(p);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(p);
        }
        Err(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
pub fn to_local(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_local_in(
        pool(),
        console,
        src,
        kind,
        dest_dir,
        unsafe_read,
        job_id,
        counters,
        cancel,
    )
}

/// `to_local` with `escape_names` (see [`to_local_with_in`]).
#[allow(clippy::too_many_arguments)]
pub fn to_local_escaped(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_local_with_in(
        pool(),
        console,
        src,
        kind,
        dest_dir,
        unsafe_read,
        true,
        job_id,
        counters,
        cancel,
    )
}

/// How a zip download stores its entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZipCompression {
    /// Uncompressed entries (the default): game data is already compressed, and the archive
    /// resumes at the byte a dropped connection reached.
    #[default]
    Stored,
    /// Deflated entries, for text-heavy trees. A deflate stream cannot be continued, so a
    /// dropped connection restarts the whole archive from zero: it cannot resume.
    Deflate,
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with_in(
        pool,
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        ZipCompression::Stored,
        job_id,
        counters,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_with_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    compression: ZipCompression,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    let name = basename(src)?.to_owned();
    let single = kind == DownloadKind::File;
    // A zip needs in-order bytes.
    let mut flags = gen::JF_ORDERED;
    if single {
        flags |= gen::JF_SINGLE_FILE;
    }
    if unsafe_read {
        flags |= gen::JF_UNSAFE_READ;
    }
    let dest = dest_zip.to_path_buf();
    let deflate = compression == ZipCompression::Deflate;
    let r = run(
        pool,
        console,
        src,
        flags,
        job_id,
        deflate,
        &move || -> Arc<dyn Sink> {
            match (deflate, single) {
                (true, true) => Arc::new(ZipSink::single(dest.clone(), &name)),
                (true, false) => Arc::new(ZipSink::new(dest.clone(), &name)),
                (false, true) => Arc::new(StoredZipSink::single(dest.clone(), &name)),
                (false, false) => Arc::new(StoredZipSink::new(dest.clone(), &name)),
            }
        },
        counters,
        cancel,
    );
    if r.is_err() {
        // Terminal (a cancel, a refusal, a local write failure, the stall limit): nothing
        // resumes this job, so its journal must not outlive it (the sink removes its part).
        let _ = std::fs::remove_dir_all(journal::job_dir(&pool.ava_dir().join("jobs"), &job_id));
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with(
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        ZipCompression::Stored,
        job_id,
        counters,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_with(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    compression: ZipCompression,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with_in(
        pool(),
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        compression,
        job_id,
        counters,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_restarted_zip_never_shows_more_than_the_whole_archive() {
        // Review 019 F3: base_* carries the work of earlier attempts, so base + durable can
        // pass the archive's own size after a restart. What the UI is shown is capped at it.
        let p = Arc::new(Progress::default());
        p.bytes_total.store(1000, Ordering::Relaxed);
        p.files_total.store(10, Ordering::Relaxed);
        p.bytes_durable.store(400, Ordering::Relaxed);
        p.files_durable.store(4, Ordering::Relaxed);
        let c = Counters::default();
        let st = TickState {
            p,
            c: c.clone(),
            base_bytes: 800, // an earlier attempt already spent 800 of them
            base_files: 8,
        };
        st.store();
        assert_eq!(c.bytes.load(Ordering::Relaxed), 1000);
        assert_eq!(c.bytes_finalized.load(Ordering::Relaxed), 1000);
        assert_eq!(c.files.load(Ordering::Relaxed), 10);
        assert_eq!(c.files_finalized.load(Ordering::Relaxed), 10);
        // Below the total the counter is the plain work done, and an unknown total caps nothing.
        let c2 = Counters::default();
        let p2 = Arc::new(Progress::default());
        p2.bytes_durable.store(5000, Ordering::Relaxed);
        TickState {
            p: p2,
            c: c2.clone(),
            base_bytes: 0,
            base_files: 0,
        }
        .store();
        assert_eq!(c2.bytes.load(Ordering::Relaxed), 5000);
    }

    use super::*;

    #[test]
    fn zip_entry_names_follow_one_rule() {
        assert_eq!(zip_entry_name(false, "Game", "a/b.bin"), "Game/a/b.bin");
        assert_eq!(zip_entry_name(true, "foo.pkg", "ignored"), "foo.pkg");
    }

    #[test]
    fn attempt_ids_differ_only_after_a_restart() {
        let id = [7u8; 16];
        assert_eq!(attempt_id(id, 0), id);
        assert_ne!(attempt_id(id, 1), id);
        assert_ne!(attempt_id(id, 1), attempt_id(id, 2));
    }

    /// Legal on the PS5; a device, a stream or another name once written on Windows.
    #[test]
    fn basename_refuses_a_root_name_this_computer_cannot_hold() {
        for src in [
            "/data/CON",
            "/data/aux.dat",
            "/data/x.",
            "/data/a:b",
            "/data/x ",
        ] {
            let e = basename(src).unwrap_err();
            let f = e.downcast_ref::<UploadFailure>().expect(src);
            assert_eq!(f.reason, "ava1_bad_manifest", "{src}");
        }
        assert_eq!(basename("/data/game.exfat").unwrap(), "game.exfat");
    }

    #[test]
    fn landed_files_keep_the_ps5_times() {
        // #812: a clip downloaded from the PS5 was dated when it arrived.
        let d = crate::test_tmp::temp_path(format!("p5a-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Clips/sub")).unwrap();
        std::fs::write(d.join("Clips/a.mp4"), b"a").unwrap();
        std::fs::write(d.join("Clips/sub/b.mp4"), b"b").unwrap();
        std::fs::write(d.join("one.png"), b"c").unwrap();
        let at = |kind, path: &str, mtime| manifest::Entry {
            mtime,
            ..entry(kind, path)
        };
        let folder = Manifest {
            entries: vec![
                at(gen::ENTRY_DIR, "sub", 1_600_000_000),
                at(gen::ENTRY_FILE, "a.mp4", 1_700_000_000),
                at(gen::ENTRY_FILE, "sub/b.mp4", 0), // unknown: left alone
            ],
        };
        let before = std::fs::metadata(d.join("Clips/sub/b.mp4"))
            .unwrap()
            .modified()
            .unwrap();
        stamp_mtimes(&d.join("Clips"), false, &folder);
        let secs = |p: &str| {
            std::fs::metadata(d.join(p))
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        assert_eq!(secs("Clips/a.mp4"), 1_700_000_000);
        assert_eq!(
            std::fs::metadata(d.join("Clips/sub/b.mp4"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        let single = Manifest {
            entries: vec![at(gen::ENTRY_FILE, "one.png", 1_650_000_000)],
        };
        stamp_mtimes(&d.join("one.png"), true, &single);
        assert_eq!(secs("one.png"), 1_650_000_000);
        // A file that is not there is skipped, never an error.
        stamp_mtimes(&d.join("gone"), true, &single);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn basename_refuses_names_that_climb() {
        assert_eq!(basename("/data/foo/").unwrap(), "foo");
        assert!(basename("/").is_err());
        assert!(basename("/a/..").is_err());
    }

    fn entry(kind: u8, path: &str) -> manifest::Entry {
        manifest::Entry {
            kind,
            mode: 0o644,
            size: 0,
            mtime: 0,
            path: path.into(),
            root: None,
        }
    }

    fn files(paths: &[&str]) -> Manifest {
        Manifest {
            entries: paths.iter().map(|p| entry(gen::ENTRY_FILE, p)).collect(),
        }
    }

    /// A console folder holding `Save.dat` and `save.dat` is refused where the disk keeps one
    /// file for both (it ended "done" with one file's bytes lost), and only there. Checked
    /// against what the disk under the temp folder really does.
    #[test]
    fn names_that_differ_only_in_case_are_refused_where_the_disk_merges_them() {
        let dir = crate::test_tmp::temp_path(format!("ps5u-dl-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("probe")).unwrap();
        let really = dir.join("PROBE").exists();
        // The download's own folder is not made yet: the one above answers.
        let root = dir.join("probe").join("save");
        assert_eq!(ignores_case(&root), really);

        let twins = files(&["a/Save.dat", "b.bin", "a/save.dat"]);
        assert_eq!(case_twins(&twins), Some(("a/Save.dat", "a/save.dat")));
        assert_eq!(refuse_case_twins(&twins, &root).is_err(), really);
        // Folders that differ in case only merge; nothing is lost.
        let dirs = Manifest {
            entries: vec![
                entry(gen::ENTRY_DIR, "Media"),
                entry(gen::ENTRY_DIR, "media"),
                entry(gen::ENTRY_FILE, "Media/a.prx"),
                entry(gen::ENTRY_FILE, "media/b.prx"),
            ],
        };
        assert_eq!(case_twins(&dirs), None);
        assert!(refuse_case_twins(&files(&["a.bin", "b.bin"]), &root).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The case question is asked of the folder the files land in, never of the one above it:
    /// at a disk's root (`/Volumes/USB`, `D:\`) that is another disk, which answered for this
    /// one (a case-respecting USB disk under a case-folding `/Volumes` refused good downloads, a
    /// folding one under a respecting root let twins overwrite each other). Without a second
    /// disk the test makes the folder above lie the same way where it can: on a disk that tells
    /// case apart, `vol` and `VOL` both exist above the download, and the old question found
    /// `VOL` and said the disk folds case.
    #[test]
    fn the_case_question_is_asked_inside_the_destination_never_above_it() {
        let dir = crate::test_tmp::temp_path(format!("ps5u-dl-case-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vol")).unwrap();
        let folds = std::fs::create_dir(dir.join("VOL")).is_err();
        // An empty folder (asked with a probe, which is gone afterwards), and the download's
        // own folder not made yet.
        assert_eq!(ignores_case(&dir.join("vol").join("Game")), folds);
        assert_eq!(std::fs::read_dir(dir.join("vol")).unwrap().count(), 0);
        // A folder of names without letters is asked with a probe too.
        std::fs::write(dir.join("vol").join("123"), b"x").unwrap();
        assert_eq!(ignores_case(&dir.join("vol").join("Game")), folds);
        // A folder with a name to ask, and the download's folder itself when it exists.
        std::fs::create_dir(dir.join("vol").join("Game")).unwrap();
        std::fs::write(dir.join("vol").join("Game").join("a.bin"), b"x").unwrap();
        assert_eq!(ignores_case(&dir.join("vol").join("Game")), folds);
        if !folds {
            // Both spellings in the folder: it tells them apart.
            std::fs::write(dir.join("vol").join("Game").join("A.BIN"), b"y").unwrap();
            assert!(!ignores_case(&dir.join("vol").join("Game")));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_shape_refuses_paths_a_host_would_misplace() {
        for bad in [
            "C:/x",
            "C:evil",
            "a/C:/x",
            "a\\b",
            "CON",
            "con",
            "a/NUL.txt",
            "Aux.tar.gz",
            "COM1",
            "lpt9.log",
            "CONIN$",
            "conout$",
            "CONIN$.txt",
            "COM\u{b9}",
            "com\u{b2}.txt",
            "LPT\u{b3}",
            "x.",
            "a/x ",
            "dir./f",
            "s:stream",
        ] {
            let e = check_shape(&files(&[bad]), false);
            assert!(e.is_err(), "{bad:?} must be refused");
            assert_eq!(e.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn check_shape_accepts_ordinary_and_lookalike_names() {
        for good in [
            "a/b.txt",
            "COM",
            "COM0",
            "COM10",
            "CONSOLE",
            "console.txt",
            "NULL",
            "LPT",
            ".hidden",
            "a b/c d",
            "x.y",
            "price$",
            "CONIN",
            "CON$X",
        ] {
            check_shape(&files(&[good]), false)
                .unwrap_or_else(|e| panic!("{good:?} must be accepted: {e}"));
        }
    }

    /// #729: keeping names takes a '\' as part of a name, so `..\..\x` is one file inside the
    /// download, while a real `..` component is still refused, and a name that could be read
    /// two ways is too.
    #[test]
    fn kept_names_never_climb_out() {
        for name in [
            "options\\gabindings.xml",
            "..\\..\\x",
            "a\\..\\..\\b",
            "C:evil",
            "CON",
        ] {
            check_shape_with(&files(&[name]), false, true)
                .unwrap_or_else(|e| panic!("{name:?} must be kept: {e}"));
            let kept = kept_manifest(&files(&[name])).unwrap();
            let p = &kept.entries[0].path;
            assert!(!p.contains(['\\', ':', '/']), "{p:?}");
            assert!(host_unsafe_component(p).is_none(), "{p:?}");
        }
        for bad in ["../x", "a/../../b", "/abs", "a//b", "a\u{F05C}b"] {
            assert!(
                check_shape_with(&files(&[bad]), false, true).is_err(),
                "{bad:?} must be refused"
            );
        }
        let e = basename_kept("/data/options\\x.xml", true).unwrap();
        assert_eq!(e, "options\u{F05C}x.xml");
        assert!(basename_kept("/data/..", true).is_err());
        assert!(basename_kept("/data/a\\b", false).is_err());
    }

    #[test]
    fn a_single_file_manifest_must_be_exactly_one_file() {
        check_shape(&files(&["one"]), true).unwrap();
        assert!(check_shape(&files(&["one", "two"]), true).is_err());
        let dir = Manifest {
            entries: vec![entry(gen::ENTRY_DIR, "d")],
        };
        assert!(
            check_shape(&dir, true).is_err(),
            "a directory is not a file"
        );
        let dir_and_file = Manifest {
            entries: vec![entry(gen::ENTRY_DIR, "d"), entry(gen::ENTRY_FILE, "d/f")],
        };
        assert!(check_shape(&dir_and_file, true).is_err());
        // A folder download may hold one file.
        check_shape(&dir_and_file, false).unwrap();
    }

    /// A sink that wraps `ZipSink` and, on its first write, reports that a file was
    /// sent again (what the receiver's FileRetry causes), once per test.
    struct RetrySink {
        inner: ZipSink,
        fail_first_write: bool,
        failed: AtomicBool,
    }

    impl Sink for RetrySink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            if self.fail_first_write && !self.failed.swap(true, Ordering::Relaxed) {
                return Err(zip_restart("injected: file sent again"));
            }
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            if self.fail_first_write && !self.failed.swap(true, Ordering::Relaxed) {
                return Err(zip_restart("injected: file sent again"));
            }
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
    }

    /// `LocalSink` whose first write fails as the OS does when the process is out of
    /// descriptors.
    struct EmfileOnce {
        inner: LocalSink,
        failed: AtomicBool,
    }

    impl EmfileOnce {
        fn fail(&self) -> io::Result<()> {
            if !self.failed.swap(true, Ordering::Relaxed) {
                return Err(io::Error::from_raw_os_error(24));
            }
            Ok(())
        }
    }

    impl Sink for EmfileOnce {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.fail()?;
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.fail()?;
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn resume_key(&self) -> Option<(String, bool)> {
            self.inner.resume_key()
        }
    }

    #[test]
    fn a_descriptor_shortage_is_waited_out_not_terminal() {
        let d = crate::test_tmp::temp_path(format!("p5a-emfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        for i in 0..3u8 {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i + 1; 5000]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let out = d.join("out/G");
        let out2 = out.clone();
        let make = move || -> Arc<dyn Sink> {
            Arc::new(EmfileOnce {
                inner: LocalSink::new(out2.clone(), false),
                failed: AtomicBool::new(false),
            })
        };
        let bytes = run(
            &pool,
            "c",
            "G",
            0,
            [8; 16],
            false,
            &make,
            &Counters::default(),
            None,
        )
        .expect("EMFILE is retried, not terminal");
        assert_eq!(bytes, 15000);
        for i in 0..3u8 {
            assert_eq!(
                std::fs::read(out.join(format!("f{i}"))).unwrap(),
                vec![i + 1; 5000]
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    async fn folder_host(dir: &Path, engine_key: [u8; 32]) -> String {
        use ava1::host::FolderHost;
        use ava1::peers::PeerStore;
        use ava1::server::{self, ServerCtx};
        let mut peers = PeerStore::in_memory();
        peers.add(engine_key, "engine").unwrap();
        let rpc: server::RpcHandler = Box::new(|_, _| ava1::session::RpcReply {
            status: gen::ERR_UNKNOWN_METHOD,
            body: Vec::new(),
        });
        let ctx = ServerCtx::new(
            ava1::keys::Identity::generate().unwrap(),
            "host",
            peers,
            rpc,
        )
        .with_jobs(Arc::new(FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        addr
    }

    // A re-sent file restarts the archive attempt (like a drop) instead of failing the
    // download as a bad manifest.
    #[test]
    fn a_zip_download_restarts_when_a_file_is_sent_again() {
        let d = crate::test_tmp::temp_path(format!("p5a-zip-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        for i in 0..3u8 {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i + 1; 5000]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let dest = d.join("g.zip");
        let made = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (made2, dest2) = (made.clone(), dest.clone());
        let make = move || -> Arc<dyn Sink> {
            let n = made2.fetch_add(1, Ordering::Relaxed);
            Arc::new(RetrySink {
                inner: ZipSink::new(dest2.clone(), "G"),
                fail_first_write: n == 0,
                failed: AtomicBool::new(false),
            })
        };
        let flags = gen::JF_ORDERED;
        // The test thread owns no runtime context: `run` blocks on its own.
        let bytes = run(
            &pool,
            "c",
            "G",
            flags,
            [9; 16],
            true,
            &make,
            &Counters::default(),
            None,
        )
        .expect("a re-sent file must restart the archive, not fail it");
        assert_eq!(bytes, 15000);
        // Two sinks were built: the original and the restart.
        assert_eq!(made.load(Ordering::Relaxed), 2);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        assert_eq!(z.len(), 3);
        for i in 0..3u8 {
            let mut e = z.by_name(&format!("G/f{i}")).unwrap();
            let mut got = Vec::new();
            io::Read::read_to_end(&mut e, &mut got).unwrap();
            assert_eq!(got, vec![i + 1; 5000]);
        }
        assert!(!d.join("g.zip.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A sink that counts every byte handed to it and, once, drops the connection when the
    /// count reaches `kill_at`: what a flaky link does mid-download.
    struct DropSink {
        inner: Arc<dyn Sink>,
        written: Arc<AtomicU64>,
        kill_at: u64,
        killed: AtomicBool,
        proxy: Arc<ava1_chaos::ChaosProxy>,
        /// Damages the part file between the drop and the resume (the 2nd `prepare`).
        damage: Option<fn(&Path)>,
        part: PathBuf,
        prepares: std::sync::atomic::AtomicU32,
        /// What the sink had been handed when the last sync that began before the drop started:
        /// the bytes a journal commit could have covered. Everything written after it, up to the
        /// resume, is what a drop may lose (and a resume then rewrites).
        synced_floor: Arc<AtomicU64>,
        /// What the sink had been handed when the resume began (its second `prepare`).
        at_resume: Arc<AtomicU64>,
    }

    impl DropSink {
        fn count(&self, n: usize) {
            let w = self.written.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
            if w >= self.kill_at && !self.killed.swap(true, Ordering::Relaxed) {
                self.proxy.kill_all();
            }
        }
    }

    impl Sink for DropSink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            if self.prepares.fetch_add(1, Ordering::Relaxed) == 1 {
                self.at_resume
                    .store(self.written.load(Ordering::Relaxed), Ordering::Relaxed);
                if let Some(f) = self.damage {
                    f(&self.part);
                }
            }
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.inner.write_at(id, off, data)?;
            self.count(data.len());
            Ok(())
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.inner.write_whole(id, data)?;
            self.count(data.len());
            Ok(())
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            let handed = self.written.load(Ordering::Relaxed);
            let before_drop = !self.killed.load(Ordering::Relaxed);
            let r = self.inner.sync(ids);
            if r.is_ok() && before_drop {
                self.synced_floor.store(handed, Ordering::Relaxed);
            }
            r
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn position(
            &self,
            done: &std::collections::BTreeSet<u32>,
            partial: &std::collections::BTreeMap<u32, ava1::ranges::RangeSet>,
        ) -> io::Result<()> {
            self.inner.position(done, partial)
        }
    }

    /// Downloads `files` x `size` bytes into a Stored zip, dropping the connection at
    /// ~40% of the bytes. Returns (bytes the sink was handed in all, total bytes, run
    /// the bytes a drop could have lost); the archive is checked against the source byte for byte.
    fn drop_at_forty_percent(
        tag: &str,
        files: usize,
        size: usize,
        bps: u64,
        damage: Option<fn(&Path)>,
    ) -> (u64, u64, u64) {
        let d = crate::test_tmp::temp_path(format!("p5a-zip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        let mut want = Vec::new();
        for i in 0..files {
            let b: Vec<u8> = (0..size).map(|k| (k * 7 + i * 13) as u8).collect();
            std::fs::write(d.join(format!("share/G/f{i:02}")), &b).unwrap();
            want.push(b);
        }
        let total = (files * size) as u64;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let host = rt.block_on(folder_host(&d, key));
        let proxy = Arc::new(
            rt.block_on(ava1_chaos::ChaosProxy::start(
                host.parse().unwrap(),
                ava1_chaos::ChaosConfig {
                    bytes_per_sec: Some(bps),
                    ..Default::default()
                },
            ))
            .unwrap(),
        );
        let pool = Pool::new(ava).with_addr(proxy.addr.to_string());
        let dest = d.join("g.zip");
        let written = Arc::new(AtomicU64::new(0));
        let synced_floor = Arc::new(AtomicU64::new(0));
        let at_resume = Arc::new(AtomicU64::new(0));
        let sink: Arc<dyn Sink> = Arc::new(DropSink {
            inner: Arc::new(StoredZipSink::new(dest.clone(), "G")),
            written: written.clone(),
            kill_at: total * 4 / 10,
            killed: AtomicBool::new(false),
            proxy: proxy.clone(),
            damage,
            part: d.join("g.zip.ava-part"),
            prepares: Default::default(),
            synced_floor: synced_floor.clone(),
            at_resume: at_resume.clone(),
        });
        let c = Counters::default();
        let bytes = run(
            &pool,
            "c",
            "G",
            gen::JF_ORDERED,
            [5; 16],
            false,
            &move || sink.clone(),
            &c,
            None,
        )
        .expect("a dropped Stored zip download must resume");
        assert_eq!(bytes, total);
        assert!(pool.attempts() >= 2, "the connection never dropped");
        assert_eq!(
            c.bytes.load(Ordering::Relaxed),
            total,
            "resumed, not restarted: the work counter never needed more than the archive"
        );
        let mut z = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        assert_eq!(z.len(), files);
        for (i, b) in want.iter().enumerate() {
            let mut e = z.by_name(&format!("G/f{i:02}")).unwrap();
            assert_eq!(e.compression(), zip::CompressionMethod::Stored);
            let mut got = Vec::new();
            io::Read::read_to_end(&mut e, &mut got).unwrap();
            assert!(&got == b, "G/f{i:02} differs");
        }
        assert!(!d.join("g.zip.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
        (
            written.load(Ordering::Relaxed),
            total,
            // The most a drop could have lost: handed before the resume, not yet synced.
            at_resume
                .load(Ordering::Relaxed)
                .saturating_sub(synced_floor.load(Ordering::Relaxed)),
        )
    }

    // Step 2: whole entries are kept. 1 MiB entries are one group each, so only the entry
    // in flight can be lost; at 4 MiB/s a sync batch holds well under an entry.
    #[test]
    fn a_stored_zip_resumes_with_at_most_one_entry_resent() {
        let (handed, total, could_lose) =
            drop_at_forty_percent("entry", 16, 1 << 20, 4 << 20, None);
        let resent = handed - total;
        // What a drop loses is what was handed to the sink since the last sync that began before
        // it, plus a commit's worth of slack (the journal record follows the sync). That is the
        // property: nothing synced is sent twice. A fixed byte bound only held on an idle host,
        // where the sync period is short next to the link's rate.
        assert!(
            resent <= could_lose + (1 << 20),
            "{resent} bytes were sent twice but only {could_lose} were unsynced at the drop"
        );
    }

    // Step 3: inside an entry. 12 MiB entries are 12 groups; the cut lands inside one, and
    // what is resent is the un-journaled tail (a sync batch), never the entry.
    #[test]
    fn a_stored_zip_resumes_mid_entry_with_at_most_a_group_or_two_resent() {
        let (handed, total, could_lose) = drop_at_forty_percent("mid", 4, 12 << 20, 8 << 20, None);
        let resent = handed - total;
        assert!(
            resent <= could_lose + (1 << 20),
            "{resent} bytes were sent twice but only {could_lose} were unsynced at the drop"
        );
        assert!(resent < 12 << 20, "the entry was sent again whole");
    }

    // The sink cannot honour the journal (the archive is shorter than the journal's cut):
    // the receiver resets the journal, the archive restarts from 0, the download completes.
    #[test]
    fn a_resume_the_sink_cannot_honour_starts_over_and_still_verifies() {
        let (handed, total, _) = drop_at_forty_percent(
            "fallback",
            4,
            12 << 20,
            8 << 20,
            Some(|p| {
                let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
                f.set_len(1000).unwrap();
            }),
        );
        // Everything sent before the drop was thrown away and sent again.
        assert!(
            handed - total >= total * 3 / 10,
            "only {} bytes were resent",
            handed - total
        );
    }

    // Bit rot inside the in-flight entry's durable bytes is caught by the receiver's
    // re-check against the group CVs (a multi-group file's outboard): the file restarts,
    // and the finished archive still matches the source.
    #[test]
    fn rot_in_an_in_flight_entrys_durable_bytes_is_caught() {
        let (handed, total, _) = drop_at_forty_percent(
            "rot",
            4,
            12 << 20,
            8 << 20,
            Some(|p| {
                // 1 MiB into the second entry: inside its durable prefix at the 40% drop.
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(p)
                    .unwrap();
                let at = (13 << 20) as u64;
                let mut b = [0u8; 1];
                std::os::unix::fs::FileExt::read_exact_at(&f, &mut b, at).unwrap();
                b[0] ^= 0xff;
                std::os::unix::fs::FileExt::write_all_at(&f, &b, at).unwrap();
            }),
        );
        assert!(handed > total, "the rotted file was not sent again");
    }

    /// Records every write so a test can check the order bytes were delivered in.
    struct OrderSink {
        inner: StoredZipSink,
        seen: Mutex<Vec<(u32, u64, usize)>>,
    }

    impl Sink for OrderSink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.seen.lock().unwrap().push((id, off, data.len()));
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.seen.lock().unwrap().push((id, 0, data.len()));
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn position(
            &self,
            done: &std::collections::BTreeSet<u32>,
            partial: &std::collections::BTreeMap<u32, ava1::ranges::RangeSet>,
        ) -> io::Result<()> {
            self.inner.position(done, partial)
        }
    }

    // An ordered job that is not resuming has no skip list: every byte of every file is
    // delivered, in (file, offset) order, with no gaps.
    #[test]
    fn a_fresh_ordered_job_delivers_every_byte_in_order() {
        let d = crate::test_tmp::temp_path(format!("p5a-ord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        let sizes = [3 << 20, 100, (2 << 20) + 17, 1 << 20, 5];
        for (i, n) in sizes.iter().enumerate() {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i as u8 + 1; *n]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let sink = Arc::new(OrderSink {
            inner: StoredZipSink::new(d.join("g.zip"), "G"),
            seen: Mutex::new(Vec::new()),
        });
        let s2: Arc<dyn Sink> = sink.clone();
        run(
            &pool,
            "c",
            "G",
            gen::JF_ORDERED,
            [6; 16],
            false,
            &move || s2.clone(),
            &Counters::default(),
            None,
        )
        .unwrap();
        let seen = sink.seen.lock().unwrap().clone();
        let mut next = (0u32, 0u64);
        let sz: Vec<u64> = (0..5)
            .map(|i| {
                std::fs::metadata(d.join(format!("share/G/f{i}")))
                    .unwrap()
                    .len()
            })
            .collect();
        for (id, off, n) in seen {
            assert_eq!((id, off), next, "delivery out of order or with a gap");
            next = if off + n as u64 == sz[id as usize] {
                (id + 1, 0)
            } else {
                (id, off + n as u64)
            };
        }
        assert_eq!(next, (5, 0), "not every byte was delivered");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_rewrite_from_offset_zero_is_a_restart_and_a_gap_is_not() {
        let d = crate::test_tmp::temp_path(format!("p5a-zip-sink-rw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut m = files(&["a", "b"]);
        m.entries[0].size = 100;
        m.entries[1].size = 50;
        let s = ZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.write_at(0, 0, &[1; 100]).unwrap();
        s.write_at(1, 0, &[2; 10]).unwrap();
        // An earlier, finished file sent again, and the current one again.
        assert!(is_zip_restart(&s.write_at(0, 0, &[1; 100]).unwrap_err()));
        assert!(is_zip_restart(&s.write_at(1, 0, &[2; 10]).unwrap_err()));
        // A gap is a protocol violation, not a retry.
        let gap = s.write_at(1, 30, &[2; 10]).unwrap_err();
        assert!(!is_zip_restart(&gap));
        assert_eq!(gap.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recoverable_local_failure_keeps_its_journal_and_staging() {
        let d = crate::test_tmp::temp_path(format!("p5a-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, part) = (d.join("job"), d.join("Game.ava-part"));
        let fill = || {
            std::fs::create_dir_all(&job).unwrap();
            std::fs::write(job.join("journal"), b"x").unwrap();
            std::fs::create_dir_all(&part).unwrap();
        };
        let local_io = terminal(SendError::Source(io::Error::other("disk full")));
        fill();
        cleanup_after_failure(&local_io, &job, &part, &d, false, false);
        assert!(job.exists() && part.exists(), "resume lost its work");
        for e in [
            anyhow!("transfer_cancelled"),
            terminal(SendError::Source(invalid("bad name"))),
            terminal(SendError::Refused {
                status: gen::ERR_PATH,
                message: "no".into(),
            }),
        ] {
            fill();
            cleanup_after_failure(&e, &job, &part, &d, false, false);
            assert!(!job.exists() && !part.exists(), "a final failure left work");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn terminal_errors_keep_their_reasons() {
        let reason = |e: SendError| {
            let e = terminal(e);
            match e.downcast_ref::<UploadFailure>() {
                Some(f) => f.reason.clone(),
                None => e.to_string(),
            }
        };
        assert_eq!(reason(SendError::Cancelled), "transfer_cancelled");
        assert_eq!(
            reason(SendError::Source(io::Error::from(
                io::ErrorKind::PermissionDenied
            ))),
            "ava1_local_io"
        );
        assert_eq!(
            reason(SendError::Source(io::Error::other("disk full"))),
            "ava1_local_io"
        );
        assert_eq!(
            reason(SendError::Source(invalid("bad name"))),
            "ava1_bad_manifest"
        );
        assert_eq!(
            reason(SendError::Refused {
                status: gen::ERR_PATH,
                message: "no".into()
            }),
            "ava1_not_allowed"
        );
        assert_eq!(
            reason(SendError::Refused {
                status: gen::ERR_PATH,
                message: "Saves/slot1 is a symbolic link".into()
            }),
            "ava1_local_path"
        );
    }

    #[test]
    fn a_stale_staging_path_is_cleared_only_for_a_fresh_journal() {
        let d = crate::test_tmp::temp_path(format!("p5a-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("j")).unwrap();
        assert!(journal_is_empty(&d.join("missing")));
        assert!(journal_is_empty(&d.join("j")));
        std::fs::write(d.join("j/rec"), b"x").unwrap();
        assert!(!journal_is_empty(&d.join("j")));
        std::fs::create_dir_all(d.join("p.ava-part/sub")).unwrap();
        remove_path(&d.join("p.ava-part"));
        assert!(!d.join("p.ava-part").exists());
        std::fs::write(d.join("f.ava-part"), b"x").unwrap();
        remove_path(&d.join("f.ava-part"));
        assert!(!d.join("f.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A folder download renamed into place whose session was lost before the console heard it
    /// was done runs again with the same sink: the journal says every file is done, and the
    /// finish must take the folder that is there as its own.
    #[test]
    fn a_retry_after_the_folder_was_renamed_into_place_finishes() {
        let d = crate::test_tmp::temp_path(format!("p5a-landed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let target = d.join("Game");
        let sink = CheckedSink {
            inner: LocalSink::new(target.clone(), false),
            root: target.clone(),
            single: false,
            escape_names: false,
            landed: Mutex::new(None),
        };
        let m = Manifest {
            entries: vec![manifest::Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 3,
                mtime: 0,
                path: "a.bin".into(),
                root: None,
            }],
        };
        assert_eq!(sink.resume_key().map(|(_, staged)| staged), Some(true));
        sink.prepare(&m).unwrap();
        sink.write_whole(0, b"abc").unwrap();
        sink.sync(&[0]).unwrap();
        sink.commit(0).unwrap();
        sink.sync_committed(&[0]).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read(target.join("a.bin")).unwrap(), b"abc");
        // The retry: the journal's Open said staged, and every file is done.
        sink.resume_staged(true, true);
        sink.prepare(&m).unwrap();
        sink.finish().expect("the folder is this job's own");
        assert_eq!(std::fs::read(target.join("a.bin")).unwrap(), b"abc");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A console folder holding both `X` and `X.ava-part`, downloaded into a folder that already
    /// exists: `X` is written as `X.ava-part` until it is complete, so the two were one file and
    /// one of them ended with the other's bytes. Refused before anything is written, and the
    /// failure's clean-up leaves the `X.ava-part` already there alone (it is the tree's, not a
    /// part file). Into a new folder (staged whole, no part names) the same tree is fine.
    #[test]
    fn a_tree_holding_x_and_x_ava_part_is_refused_into_an_existing_folder_only() {
        let d = crate::test_tmp::temp_path(format!("p5a-partname-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, target) = (d.join("job"), d.join("Game"));
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let file = |path: &str| manifest::Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size: 5 << 20,
            mtime: 0,
            path: path.into(),
            root: None,
        };
        let sink = |root: &Path| CheckedSink {
            inner: LocalSink::new(root.to_path_buf(), false),
            root: root.to_path_buf(),
            single: false,
            escape_names: false,
            landed: Mutex::new(None),
        };
        for m in [
            Manifest {
                entries: vec![file("a/X"), file("a/X.ava-part")],
            },
            Manifest {
                entries: vec![file("a/X.ava-part"), file("a/X")],
            },
        ] {
            let e = sink(&target).prepare(&m).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
            assert!(e.to_string().contains("X.ava-part"), "{e}");
            // A new folder is staged: nothing there is written under a part name.
            sink(&d.join("New")).prepare(&m).unwrap();
        }
        // Different names are no clash.
        let fine = Manifest {
            entries: vec![file("a/X"), file("a/Y.ava-part"), file("X.ava-part")],
        };
        sink(&target).prepare(&fine).unwrap();

        // The refusal is final: the clean-up runs on the saved manifest, and the tree's own
        // `X.ava-part` (here from an earlier download) is not X's part file.
        let m = Manifest {
            entries: vec![file("a/X"), file("a/X.ava-part"), file("a/Y")],
        };
        journal::write_manifest(&job, &m).unwrap();
        std::fs::create_dir_all(target.join("a")).unwrap();
        std::fs::write(target.join("a/X.ava-part"), b"real").unwrap();
        std::fs::write(target.join("a/Y.ava-part"), b"p").unwrap();
        let part = d.join("Game.ava-part");
        let refused = terminal(SendError::Source(invalid("clash")));
        cleanup_after_failure(&refused, &job, &part, &target, false, false);
        assert_eq!(std::fs::read(target.join("a/X.ava-part")).unwrap(), b"real");
        assert!(
            !target.join("a/Y.ava-part").exists(),
            "Y's part file is the job's"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The clean-up after a failed download removes part files at the paths of the journal's
    /// saved manifest, which is saved before `prepare` checks it: a refused manifest comes back
    /// here. Its hostile paths (on Windows `..\x` climbs, `C:x` and `C:\x` name another place, a
    /// device name is no file) are skipped by the writer's own checks on every host, so the test
    /// means the same on this one.
    #[test]
    fn the_failure_clean_up_removes_nothing_at_a_path_the_writer_refuses() {
        let file = |path: &str| manifest::Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size: 5 << 20,
            mtime: 0,
            path: path.into(),
            root: None,
        };
        let hostile = [
            "..\\x",
            "a\\..\\..\\x",
            "C:x",
            "C:\\Windows\\x",
            "\\\\server\\share\\x",
            "\\x",
            "/abs/x",
            "../x",
            "a/../../x",
            "CON",
            "a/NUL.txt",
            "x.",
            "a/b:stream",
        ];
        let mut entries: Vec<_> = hostile.iter().map(|p| file(p)).collect();
        entries.push(file("ok/y"));
        let m = Manifest { entries };
        assert_eq!(part_files(&m, false), vec!["ok/y.ava-part".to_string()]);
        // Kept names: whatever the escape makes of them stays one name inside the folder.
        for p in part_files(&m, true) {
            assert!(
                !p.contains('\\') && !p.contains(':') && !p.starts_with('/'),
                "{p:?}"
            );
            assert!(p.split('/').all(|c| c != ".." && c != "."), "{p:?}");
        }

        // And through the real clean-up: only the safe file's part goes.
        let d = crate::test_tmp::temp_path(format!("p5a-hostile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, target) = (d.join("job"), d.join("in").join("Game"));
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(target.join("ok")).unwrap();
        journal::write_manifest(&job, &m).unwrap();
        std::fs::write(target.join("ok/y.ava-part"), b"p").unwrap();
        std::fs::write(d.join("x.ava-part"), b"not the job's").unwrap();
        std::fs::write(d.join("in").join("x.ava-part"), b"not the job's").unwrap();
        cleanup_after_failure(
            &anyhow!("transfer_cancelled"),
            &job,
            &d.join("in").join("Game.ava-part"),
            &target,
            false,
            false,
        );
        assert!(!target.join("ok/y.ava-part").exists());
        assert!(d.join("x.ava-part").exists());
        assert!(d.join("in").join("x.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_final_failure_into_an_existing_folder_removes_its_in_place_part_files_only() {
        let d = crate::test_tmp::temp_path(format!("p5a-inplace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, target) = (d.join("job"), d.join("Game"));
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(target.join("sub")).unwrap();
        let file = |path: &str| manifest::Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size: 5 << 20,
            mtime: 0,
            path: path.into(),
            root: None,
        };
        let m = Manifest {
            entries: vec![file("big.bin"), file("sub/big2.bin")],
        };
        journal::write_manifest(&job, &m).unwrap();
        std::fs::write(job.join("journal"), b"x").unwrap();
        // what a failed run left: the job's part files, the final file it would replace, and a
        // part file that is not the job's
        std::fs::write(target.join("big.bin.ava-part"), b"p").unwrap();
        std::fs::write(target.join("sub/big2.bin.ava-part"), b"p").unwrap();
        std::fs::write(target.join("big.bin"), b"old").unwrap();
        std::fs::write(target.join("other.ava-part"), b"mine").unwrap();
        let part = d.join("Game.ava-part"); // staging: absent, the folder existed
        let local_io = terminal(SendError::Source(io::Error::other("disk full")));
        cleanup_after_failure(&local_io, &job, &part, &target, false, false);
        assert!(
            target.join("big.bin.ava-part").exists(),
            "a recoverable failure keeps its work"
        );
        cleanup_after_failure(
            &anyhow!("transfer_cancelled"),
            &job,
            &part,
            &target,
            false,
            false,
        );
        assert!(!target.join("big.bin.ava-part").exists());
        assert!(!target.join("sub/big2.bin.ava-part").exists());
        assert!(!job.exists());
        assert_eq!(std::fs::read(target.join("big.bin")).unwrap(), b"old");
        assert!(target.join("other.ava-part").exists(), "not the job's");
        let _ = std::fs::remove_dir_all(&d);
    }
}
