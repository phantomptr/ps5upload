//! The receiver on an engine (SPEC.md §12–§15): downloads, relays, the folder host.
//!
//! The Rust mirror of the payload's C receiver (ava1_recv.c, ava1_apply.c), simplified
//! by a normal OS underneath: blocking writes on `spawn_blocking`, one sync batch every
//! 250 ms run on a task of its own — the run loop keeps draining its inbox and answering
//! the peer while the batch's fsyncs run (ruling 11) — the same journal (SPEC.md §14)
//! and outboards, the same on-disk layout — so an engine can resume a job the console
//! started and vice versa.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::gen::{
    self, Bundle, Chunk, Credit, Durable, FileRange, FileRoot, JnlBatch, JnlOpen, JobCancel,
    JobDone, JobOpen, JobOpenAck, ManifestEnd, ManifestPage, Received, RootItem,
};
use crate::journal::{self, Journal, Record, State};
use crate::manifest::{self, Manifest};
use crate::packlog::{Loc, LoggedGroup, PackLog, PackOpts};
use crate::ranges::{runs, Need, RangeSet};
use crate::router::{ConnTx, Inbound, JobLink};
use crate::send::{next_ctl, Progress, SendError};
use crate::verify::{self, Outboard, GROUP};
use crate::wire::FrameMessage;

/// Where a received job's bytes land. One implementation today (`LocalSink`); a relay
/// (Task 24) and the zip sink implement their own.
pub trait Sink: Send + Sync {
    /// Called once with the manifest, before any data (create directories, decide staging).
    fn prepare(&self, m: &Manifest) -> io::Result<()>;
    /// Bytes of a large file at `off` (group-aligned, whole groups unless it ends the file).
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()>;
    /// A whole small file (its root was checked when it arrived).
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()>;
    /// `write_whole` when the caller already holds the file's BLAKE3 root (the pack log records it
    /// and need not hash the bytes again).
    fn write_whole_root(&self, id: u32, _root: &[u8; 32], data: &[u8]) -> io::Result<()> {
        self.write_whole(id, data)
    }
    /// Durability: the bytes of every file in `ids` must reach the disk before the return.
    fn sync(&self, ids: &[u32]) -> io::Result<()>;
    /// Durable-by-log (SPEC.md §15.7): turns the pack log on in the job directory `dir`. Called
    /// once, before any data. A sink with no log ignores it.
    fn enable_log(&self, _dir: &Path) {}
    /// One batch's durability: the large files `large` as in `sync`, and for the small files
    /// `small` either the same (an empty answer) or, with the log on, one fsync of the log and one
    /// group per pack segment the batch's records sit in, for the journal's pack extension.
    fn sync_batch(&self, small: &[u32], large: &[u32]) -> io::Result<Vec<LoggedGroup>> {
        let all: Vec<u32> = small.iter().chain(large).copied().collect();
        self.sync(&all)?;
        Ok(Vec::new())
    }
    /// The batch's records are in the journal: its files are done and wait for the sweep.
    fn batch_journaled(&self, _groups: &[LoggedGroup]) {}
    /// Files done but not yet durable in place.
    fn unswept(&self) -> usize {
        0
    }
    /// The unswept cap is reached: the next batch sweeps everything.
    fn log_pressure(&self) -> bool {
        false
    }
    /// Makes the logged files that are due (every one when `force`) durable in place; their ids.
    fn sweep(&self, _force: bool) -> io::Result<Vec<u32>> {
        Ok(Vec::new())
    }
    /// The `JnlSweep` for `ids` is durable: they stop holding their pack segments.
    fn sweep_journaled(&self, _ids: &[u32]) {}
    /// Crash recovery: re-makes the files `st` calls unswept from the pack and queues them for the
    /// sweep. Returns the files whose record could not be found (they are reset and resent).
    fn recover_log(&self, _st: &State) -> io::Result<Vec<u32>> {
        Ok(Vec::new())
    }
    /// Nothing is unswept: removes the pack files.
    fn log_cleanup(&self) {}
    /// The resume check's read (SPEC.md §13.4).
    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize>;
    /// A relay keeps no durable bytes. Its need hint comes from the destination
    /// receiver, which verifies the complete file; this receiver only forwards.
    fn transient_relay(&self) -> bool {
        false
    }
    /// A complete file: part → final, same directory.
    fn commit(&self, id: u32) -> io::Result<()>;
    /// Every file of `ids` has been committed (renamed into place): makes the new names durable
    /// (their directories fsynced) before the journal record that calls them done (review 009 #5).
    /// A sink with no directories ignores it.
    fn sync_committed(&self, _ids: &[u32]) -> io::Result<()> {
        Ok(())
    }
    /// The whole job: staging → final.
    fn finish(&self) -> io::Result<()>;
    /// What the journal's Open records (SPEC.md §14): (destination root, staged). `None`
    /// for a sink with nothing resumable on disk. LocalSink returns the C receiver's rule
    /// (`staged = !single_file && !root.exists()`); see ruling Q1.
    fn resume_key(&self) -> Option<(String, bool)> {
        None
    }
    /// Called once per run, after `prepare` and the §13.4 re-check and before the map goes
    /// out, with what the journal says is durable: the finished files and the partial
    /// files' ranges. A sink whose bytes live in one ordered stream (the Stored zip) cuts
    /// that stream back to exactly this state here, so what the sender is told matches
    /// what the sink holds. An `Err` means the sink cannot honour it: the receiver drops
    /// the journal, starts the job over and calls this again with nothing durable (which
    /// must then succeed). Not called for a relay (it keeps no durable bytes).
    fn position(
        &self,
        _done: &BTreeSet<u32>,
        _partial: &BTreeMap<u32, RangeSet>,
    ) -> io::Result<()> {
        Ok(())
    }
}

/// The root of a file of at most one group, as the sink now holds it. A zero-length file is the
/// empty root with nothing to read back: no sink is asked to read a file that has no bytes (a
/// zip sink cannot, and a local one would look for a part file that never existed).
fn one_group_root(sink: &dyn Sink, id: u32, size: u64) -> Option<[u8; 32]> {
    if size == 0 {
        return Some(*blake3::hash(&[]).as_bytes());
    }
    let mut buf = vec![0u8; size as usize];
    sink.read_at(id, 0, &mut buf)
        .ok()
        .map(|_| *blake3::hash(&buf).as_bytes())
}

/// Whether a `LocalSink` takes the durable-by-log path unless told otherwise. The environment decides
/// (`PS5UPLOAD_AVA1_LOG_SMALL=1` / `0`); with no setting it is on everywhere but macOS, where a plain
/// fsync never reaches the drive (one `F_FULLFSYNC` per batch already covers the files) so the log
/// only adds a second write: measured on loopback, 2,000 tiny files download at ~2,300 files/s with
/// the log against ~2,900 without. The log is for drives where a per-file fsync costs (the console's,
/// Linux and Windows disks).
fn log_small_default() -> bool {
    match std::env::var("PS5UPLOAD_AVA1_LOG_SMALL") {
        Ok(v) => v != "0",
        Err(_) => !cfg!(target_vendor = "apple"),
    }
}

/// Files under `root` on this computer: new folders staged in `<root>.ava-part`, large
/// files through `<name>.ava-part`, one rename each at the end.
pub struct LocalSink {
    root: PathBuf,
    single: bool,
    /// The staging decision, taken once at construction — the C receiver's rule
    /// (`staged = !single_file && !root.exists()`, ava1_recv.c:380-430). It is stable
    /// across a crash because the only transition is the atomic part→final rename, so
    /// `root absent ⟺ part present` on every run (ruling 10).
    staged: bool,
    st: Mutex<LocalState>,
    /// Durable-by-log: whether small files go through the pack log (default on; the environment's
    /// `PS5UPLOAD_AVA1_LOG_SMALL=0` keeps the per-file fsync path), its options, and the log itself
    /// once `enable_log` has set it up.
    log: bool,
    pack_opts: PackOpts,
    pack: Mutex<Option<PackLog>>,
    /// Test seam: runs inside `commit` after the state lock is released, where the fsync and the
    /// rename happen.
    #[cfg(test)]
    commit_hook: Mutex<Option<Box<dyn Fn(u32) + Send + Sync>>>,
}

#[derive(Default)]
struct LocalState {
    m: Option<Arc<Manifest>>,
    open: HashMap<u32, std::fs::File>,
    /// Final paths whose commit (fsync + rename) is running with the lock released: a second
    /// commit of the same path is refused instead of racing the first.
    committing: BTreeSet<PathBuf>,
}

/// Frees a path of `LocalState::committing` when its commit ends, however it ends.
struct CommitGuard<'a> {
    sink: &'a LocalSink,
    fin: PathBuf,
}

impl Drop for CommitGuard<'_> {
    fn drop(&mut self) {
        self.sink
            .st
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .committing
            .remove(&self.fin);
    }
}

impl LocalSink {
    pub fn new(root: PathBuf, single_file: bool) -> Self {
        let staged = !single_file && !root.exists();
        Self {
            root,
            single: single_file,
            staged,
            st: Mutex::default(),
            log: log_small_default(),
            pack_opts: PackOpts::default(),
            pack: Mutex::new(None),
            #[cfg(test)]
            commit_hook: Mutex::new(None),
        }
    }

    /// Chooses the durable-by-log path explicitly (tests, and the fallback switch).
    pub fn with_log(mut self, on: bool, opts: PackOpts) -> Self {
        self.log = on;
        self.pack_opts = opts;
        self
    }

    /// The small file's bytes in place, no fsync and no descriptor kept (the log holds them).
    fn make_file(&self, id: u32, data: &[u8]) -> io::Result<()> {
        let p = {
            let st = self.st.lock().unwrap();
            self.path(&st, id, false)
        };
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&p)?;
        verify::write_all_at(&f, data, 0)?;
        f.set_len(data.len() as u64)
    }

    fn write_logged(&self, id: u32, root: [u8; 32], data: &[u8]) -> io::Result<()> {
        let rec = gen::BundleRecord {
            file_id: id,
            root,
            data: data.to_vec(),
        };
        let loc = {
            let mut g = self.pack.lock().unwrap();
            g.as_mut().expect("checked by the caller").append(&rec)?
        };
        let r = self.make_file(id, data);
        if r.is_err() {
            if let Some(p) = self.pack.lock().unwrap().as_mut() {
                p.forget(loc);
            }
        }
        r
    }

    /// Makes the files of `due` right in place and durable: re-made from their record when missing
    /// or the wrong size, fsynced, one drive-cache flush, their directories synced.
    fn sweep_files(&self, due: &[Loc]) -> io::Result<()> {
        let m = self
            .st
            .lock()
            .unwrap()
            .m
            .clone()
            .ok_or_else(|| io::Error::other("sweep before prepare"))?;
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut last: Option<std::fs::File> = None;
        for l in due {
            let p = {
                let st = self.st.lock().unwrap();
                self.path(&st, l.id, false)
            };
            let size = m.entry(l.id).map_or(0, |e| e.size);
            let open = || std::fs::OpenOptions::new().write(true).open(&p);
            let f = match open() {
                Ok(f) if f.metadata().map(|md| md.len() == size).unwrap_or(false) => f,
                _ => {
                    let rec = {
                        let g = self.pack.lock().unwrap();
                        g.as_ref()
                            .ok_or_else(|| io::Error::other("no pack log"))?
                            .read(l)?
                    };
                    self.make_file(l.id, &rec.data)?;
                    open()?
                }
            };
            sys_fsync(&f)?;
            if let Some(parent) = p.parent() {
                dirs.insert(parent.to_path_buf());
            }
            last = Some(f);
        }
        if let Some(f) = &last {
            flush_drive_cache(f)?;
        }
        sync_dirs(&dirs)
    }

    fn base(&self) -> PathBuf {
        if self.staged {
            PathBuf::from(format!("{}.ava-part", self.root.display()))
        } else {
            self.root.clone()
        }
    }

    /// The path for `id`, part or final. Every `.ava-part` path is derived from the final
    /// path's own parent, so the part→final renames are same-directory by construction and
    /// can never cross a device (Global Constraint 60; ruling Q3: the placement is the
    /// guard — the payload's C keeps the `st_dev` check, a host OS returns EXDEV).
    fn path(&self, st: &LocalState, id: u32, part: bool) -> PathBuf {
        if self.single {
            return if part {
                PathBuf::from(format!("{}.ava-part", self.root.display()))
            } else {
                self.root.clone()
            };
        }
        let rel = &st
            .m
            .as_ref()
            .expect("prepare runs before any data")
            .entry(id)
            .expect("an id the receiver validated against the manifest")
            .path;
        let p = self.base().join(rel);
        if part && !self.staged {
            PathBuf::from(format!("{}.ava-part", p.display()))
        } else {
            p
        }
    }

    fn file(&self, id: u32, part: bool, truncate: bool) -> io::Result<std::fs::File> {
        let mut st = self.st.lock().unwrap();
        if !truncate {
            if let Some(f) = st.open.get(&id) {
                return f.try_clone();
            }
        }
        let p = self.path(&st, id, part);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .read(true)
                .truncate(truncate)
                .open(&p)
        };
        // Descriptors are bounded: past the cap the cache drops its other entries (their data is
        // fsynced later through a reopen, see `sync`), and the process running out anyway is
        // answered the same way once before the error is reported.
        if st.open.len() >= MAX_OPEN {
            st.open.clear();
        }
        let f = match open() {
            Err(e) if is_fd_exhausted(&e) => {
                st.open.clear();
                open()?
            }
            r => r?,
        };
        st.open.insert(id, f.try_clone()?);
        Ok(f)
    }

    /// A descriptor for `id` to fsync: the cached one, or the file reopened by path (its part
    /// file, else its final place) when it was written whole or evicted from the cache.
    fn reopen(&self, id: u32) -> io::Result<(std::fs::File, PathBuf)> {
        let (fin, part) = {
            let st = self.st.lock().unwrap();
            if let Some(f) = st.open.get(&id) {
                let p = self.path(&st, id, true);
                return Ok((f.try_clone()?, p));
            }
            (self.path(&st, id, false), self.path(&st, id, true))
        };
        let open = |p: &Path| std::fs::OpenOptions::new().write(true).open(p);
        // The part file first: a large file mid-write lives there, while a small file written
        // whole has no part file and is at its final path.
        match open(&part) {
            Ok(f) => Ok((f, part)),
            Err(e) if e.kind() == io::ErrorKind::NotFound && part != fin => {
                open(&fin).map(|f| (f, fin))
            }
            Err(e) => Err(e),
        }
    }
}

/// The most files a `LocalSink` keeps open at once (large files being written; a small file's
/// descriptor is closed as soon as its bytes are written).
const MAX_OPEN: usize = 64;

/// Whether `e` is the process or the system running out of file descriptors. That is a
/// condition to wait out and retry, not a verdict on the download.
pub fn is_fd_exhausted(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        // EMFILE, ENFILE
        matches!(e.raw_os_error(), Some(24) | Some(23))
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES
        matches!(e.raw_os_error(), Some(4))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// Plain `fsync(2)`: on macOS it hands the data to the drive without flushing the drive's
/// own cache (that is `flush_drive_cache`'s one call per batch); elsewhere it is the full
/// durable sync.
#[cfg(unix)]
fn sys_fsync(f: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn fsync(fd: i32) -> i32;
    }
    loop {
        // SAFETY: fsync on a descriptor this File owns for the duration of the call.
        if unsafe { fsync(f.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(not(unix))]
fn sys_fsync(f: &std::fs::File) -> io::Result<()> {
    f.sync_data()
}

/// macOS only: `F_FULLFSYNC` flushes the drive's write cache for everything fsync'd before
/// it, so one call per batch makes the whole batch durable. A no-op where fsync already is.
#[cfg(target_vendor = "apple")]
fn flush_drive_cache(f: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_FULLFSYNC: i32 = 51;
    // SAFETY: fcntl(F_FULLFSYNC) takes no argument and only reads the descriptor.
    if unsafe { fcntl(f.as_raw_fd(), F_FULLFSYNC) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_vendor = "apple"))]
fn flush_drive_cache(_f: &std::fs::File) -> io::Result<()> {
    Ok(())
}

/// fsyncs each directory (so the new names are durable). They are independent descriptors, so
/// a game-sized tree's tens of directories go four at a time instead of one after another
/// (review 003 §3.3 item 1, the engine's side of the console's striped directory syncs).
#[cfg(unix)] // a directory cannot be opened for sync on Windows
fn sync_dirs(dirs: &BTreeSet<PathBuf>) -> io::Result<()> {
    const WAYS: usize = 4;
    let list: Vec<&PathBuf> = dirs.iter().collect();
    if list.len() <= 2 {
        return list
            .iter()
            .try_for_each(|d| sys_fsync(&std::fs::File::open(d)?));
    }
    let mut first_err: Option<io::Error> = None;
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..WAYS.min(list.len()))
            .map(|k| {
                let list = &list;
                s.spawn(move || {
                    list.iter()
                        .skip(k)
                        .step_by(WAYS)
                        .try_for_each(|d| sys_fsync(&std::fs::File::open(d)?))
                })
            })
            .collect();
        for h in handles {
            if let Err(e) = h.join().expect("a directory sync thread panicked") {
                first_err.get_or_insert(e);
            }
        }
    });
    first_err.map_or(Ok(()), Err)
}

#[cfg(not(unix))]
fn sync_dirs(_dirs: &BTreeSet<PathBuf>) -> io::Result<()> {
    Ok(())
}

impl Sink for LocalSink {
    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        st.m = Some(Arc::new(m.clone()));
        if !self.single {
            let base = self.base();
            std::fs::create_dir_all(&base)?;
            for e in m.entries.iter().filter(|e| e.kind == gen::ENTRY_DIR) {
                std::fs::create_dir_all(base.join(&e.path))?;
            }
        }
        Ok(())
    }

    fn write_whole_root(&self, id: u32, root: &[u8; 32], data: &[u8]) -> io::Result<()> {
        if self.log && self.pack.lock().unwrap().is_some() {
            return self.write_logged(id, *root, data);
        }
        self.write_whole(id, data)
    }

    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        let f = self.file(id, true, false)?;
        verify::write_all_at(&f, data, off)
    }

    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        if self.log && self.pack.lock().unwrap().is_some() {
            return self.write_logged(id, *blake3::hash(data).as_bytes(), data);
        }
        // Not cached: a folder of tens of thousands of small files must not hold a descriptor
        // each. `sync` reopens the file to fsync it.
        self.st.lock().unwrap().open.remove(&id);
        self.make_file(id, data)
    }

    fn sync(&self, ids: &[u32]) -> io::Result<()> {
        // One sync per batch, not one drive flush per file (T28): every file gets the
        // cheap fsync, then ONE drive-cache flush covers them all, then the directories
        // (so the new names are durable too). std's `sync_data` is F_FULLFSYNC on macOS —
        // ~15 ms a file, which capped a 2,000-file download at 56 files/s. Each file is
        // reopened (or its cached descriptor used) and dropped before the next, so a batch of
        // any size holds one descriptor of its own.
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut last: Option<std::fs::File> = None;
        for &id in ids {
            let (f, p) = self.reopen(id)?;
            sys_fsync(&f)?;
            if let Some(parent) = p.parent() {
                dirs.insert(parent.to_path_buf());
            }
            last = Some(f);
        }
        if let Some(f) = &last {
            flush_drive_cache(f)?;
        }
        sync_dirs(&dirs)
    }

    fn enable_log(&self, dir: &Path) {
        // Always: recovery of a journal that replays unswept files needs the log's files whatever this
        // run does with new ones. `self.log` decides only whether small files are written to it.
        *self.pack.lock().unwrap() = Some(PackLog::new(dir, self.pack_opts));
    }

    fn sync_batch(&self, small: &[u32], large: &[u32]) -> io::Result<Vec<LoggedGroup>> {
        if !self.log || self.pack.lock().unwrap().is_none() {
            let all: Vec<u32> = small.iter().chain(large).copied().collect();
            self.sync(&all)?;
            return Ok(Vec::new());
        }
        // Large files: their data and the directory entries of their part files, as before.
        if !large.is_empty() {
            self.sync(large)?;
        }
        // Small files: one fsync of the log. No file and no directory is synced here (the sweep does).
        let (fds, groups) = self
            .pack
            .lock()
            .unwrap()
            .as_mut()
            .expect("checked above")
            .take_batch(small)?;
        for f in &fds {
            sys_fsync(f)?;
        }
        if let Some(f) = fds.last() {
            flush_drive_cache(f)?;
        }
        Ok(groups)
    }

    fn batch_journaled(&self, groups: &[LoggedGroup]) {
        if let Some(p) = self.pack.lock().unwrap().as_mut() {
            p.journaled(groups);
        }
    }

    fn unswept(&self) -> usize {
        self.pack
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |p| p.unswept())
    }

    fn log_pressure(&self) -> bool {
        self.pack
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.unswept_bytes() >= p.opts().max_unswept)
    }

    fn sweep(&self, force: bool) -> io::Result<Vec<u32>> {
        let due = match self.pack.lock().unwrap().as_mut() {
            Some(p) => p.due(force, 512),
            None => return Ok(Vec::new()),
        };
        if due.is_empty() {
            return Ok(Vec::new());
        }
        match self.sweep_files(&due) {
            Ok(()) => Ok(due.iter().map(|l| l.id).collect()),
            Err(e) => {
                if let Some(p) = self.pack.lock().unwrap().as_mut() {
                    p.due_failed(&due);
                }
                Err(e)
            }
        }
    }

    fn sweep_journaled(&self, ids: &[u32]) {
        if let Some(p) = self.pack.lock().unwrap().as_mut() {
            p.swept(ids);
        }
    }

    fn recover_log(&self, st: &State) -> io::Result<Vec<u32>> {
        let m = self.st.lock().unwrap().m.clone();
        let mut g = self.pack.lock().unwrap();
        let Some(p) = g.as_mut() else {
            return Ok(Vec::new());
        };
        let m = m.ok_or_else(|| io::Error::other("recovery before prepare"))?;
        p.recover(st, |rec| {
            let Some(e) = m.entry(rec.file_id) else {
                return Ok(false);
            };
            if e.size != rec.data.len() as u64 {
                return Ok(false);
            }
            let path = {
                let st = self.st.lock().unwrap();
                self.path(&st, rec.file_id, false)
            };
            let same = std::fs::read(&path)
                .map(|b| *blake3::hash(&b).as_bytes() == rec.root)
                .unwrap_or(false);
            if !same {
                self.make_file(rec.file_id, &rec.data)?;
            }
            Ok(true)
        })
    }

    fn log_cleanup(&self) {
        if let Some(p) = self.pack.lock().unwrap().as_mut() {
            p.cleanup();
        }
    }

    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let p = {
            let st = self.st.lock().unwrap();
            self.path(&st, id, true)
        };
        let f = std::fs::File::open(p)?;
        verify::read_exact_at(&f, buf, off).map(|_| buf.len())
    }

    fn commit(&self, id: u32) -> io::Result<()> {
        // The state lock is held only to take the descriptor and name the paths: the fsync and the
        // rename of one large file must not stall every other sink call (writes of other files,
        // `sync`, reads). Nothing reads `st.open[id]` once the file is removed from it; a second
        // commit of the same path is refused while this one runs (`committing`).
        let (size, part, fin, cached, _guard) = {
            let mut st = self.st.lock().unwrap();
            let size =
                st.m.as_ref()
                    .expect("prepare runs before any data")
                    .entry(id)
                    .expect("an id the receiver validated against the manifest")
                    .size;
            let (part, fin) = (self.path(&st, id, true), self.path(&st, id, false));
            if !st.committing.insert(fin.clone()) {
                return Err(io::Error::other(format!(
                    "a commit of {} is already in flight",
                    fin.display()
                )));
            }
            let guard = CommitGuard {
                sink: self,
                fin: fin.clone(),
            };
            (size, part, fin, st.open.remove(&id), guard)
        };
        #[cfg(test)]
        if let Some(h) = self.commit_hook.lock().unwrap().as_ref() {
            h(id);
        }
        // The cached descriptor, or (the cache having been trimmed) the part file reopened.
        let f = match cached {
            Some(f) => Some(f),
            None => match std::fs::OpenOptions::new().write(true).open(&part) {
                Ok(f) => Some(f),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            },
        };
        if let Some(f) = f {
            f.set_len(size)?;
            // Cheap fsync only: the batch's journal append (sync_all) flushes the drive cache
            // once for every file this batch committed.
            sys_fsync(&f)?;
        }
        if part != fin {
            std::fs::rename(&part, &fin)?; // same directory by construction (ruling Q3)
        }
        Ok(())
    }

    fn sync_committed(&self, ids: &[u32]) -> io::Result<()> {
        let dirs: BTreeSet<PathBuf> = {
            let st = self.st.lock().unwrap();
            ids.iter()
                .filter_map(|&id| self.path(&st, id, false).parent().map(Path::to_path_buf))
                .collect()
        };
        sync_dirs(&dirs)
    }

    fn finish(&self) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        st.open.clear(); // small files were synced in their batches
        if self.staged {
            if self.root.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} appeared during the download; the files are in .ava-part",
                        self.root.display()
                    ),
                ));
            }
            std::fs::rename(self.base(), &self.root)?; // same directory by construction
        }
        Ok(())
    }

    fn resume_key(&self) -> Option<(String, bool)> {
        Some((self.root.to_string_lossy().into_owned(), self.staged))
    }
}

pub struct RecvOptions {
    pub credit: u64,
    /// The job's flags; recorded in the journal's Open so a job reopened with different
    /// flags is refused/replaced exactly as the C receiver does (ruling Q4).
    pub flags: u32,
    pub jobs_dir: PathBuf,
    /// Must equal `flags & JF_ORDERED != 0`.
    pub ordered: bool,
    pub progress: Arc<Progress>,
    pub cancel: Arc<AtomicBool>,
    /// How long a job may go without any progress (a data frame, a root, a finished disk
    /// batch or write) while the sender still owes bytes before the receiver ends it with
    /// `ERR_STALLED` (review 006 #2). `None` = `PROGRESS_DEADLINE` (3 x the default
    /// `dead_after`), or `RESUME_PROGRESS_DEADLINE` for a resumed job.
    pub progress_deadline: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct RecvReport {
    pub files: u32,
    pub bytes: u64,
    pub manifest: Arc<Manifest>,
}

fn proto(e: impl std::fmt::Display) -> SendError {
    SendError::Protocol(e.to_string())
}

/// `PathError` → wire mapping (ruling 13, documented in `manifest.rs`): a gap is a
/// malformed manifest (`ERR_PROTOCOL`); every other variant is an invalid path
/// (`ERR_PATH`). Used for manifest validation on the receiver path.
fn wire_path(e: manifest::PathError) -> SendError {
    let status = match e {
        manifest::PathError::Gap(_) | manifest::PathError::SizeOverflow => gen::ERR_PROTOCOL,
        _ => gen::ERR_PATH,
    };
    SendError::Refused {
        status,
        message: e.to_string(),
    }
}

/// The next inbox event: the session ending is an error (a job that stops reading is the
/// backpressure; an ended session ends the job).
async fn next(link: &mut JobLink) -> Result<Inbound, SendError> {
    match link.rx.recv().await {
        Some(Inbound::Closed(why)) => Err(SendError::Disconnected(why)),
        None => Err(SendError::Disconnected("the session ended".into())),
        Some(ev) => Ok(ev),
    }
}

/// Collects manifest pages until ManifestEnd; checks the end. The receiver learns the
/// manifest from the peer every time — the on-disk `manifest` file is written for the C
/// side's resume, never read back by Rust (ruling 20). The responder acks before its
/// pages, the opener's ack confirms the open, so a JobOpenAck may come first either way.
async fn read_manifest(link: &mut JobLink) -> Result<Manifest, SendError> {
    let mut pages = Vec::new();
    loop {
        let f = next_ctl(link).await?;
        match f.ty {
            ManifestPage::TYPE => pages.push(f.decode::<ManifestPage>().map_err(proto)?),
            ManifestEnd::TYPE => {
                let e: ManifestEnd = f.decode().map_err(proto)?;
                let m = Manifest::from_pages(pages).map_err(wire_path)?;
                if m.files() != e.files || m.bytes() != e.bytes || m.hash() != e.manifest_hash {
                    return Err(SendError::Protocol(
                        "the manifest does not match its end".into(),
                    ));
                }
                return Ok(m);
            }
            JobOpenAck::TYPE => {
                let a: JobOpenAck = f.decode().map_err(proto)?;
                if a.status != gen::STATUS_OK {
                    return Err(SendError::Refused {
                        status: a.status,
                        message: a.message.unwrap_or_default(),
                    });
                }
            }
            _ => {}
        }
    }
}

/// The opener side of a download (SPEC.md §11.5): JobOpen{JOB_DOWNLOAD, root = the source
/// on the peer, ext credit = the grant} → manifest. The lanes are joined first, so they
/// are already up when the responder adopts them.
pub async fn download_open(
    link: &mut JobLink,
    src_root: &str,
    flags: u32,
    credit: u64,
) -> Result<Arc<Manifest>, SendError> {
    if let Some(op) = link.opener().cloned() {
        while link.lanes().len() < crate::governor::START_LANES as usize {
            op.open()
                .await
                .map_err(|e| SendError::Disconnected(e.to_string()))?;
        }
    }
    link.control
        .send(&JobOpen {
            job_id: link.job_id,
            kind: gen::JOB_DOWNLOAD,
            policy: 0,
            flags,
            root: src_root.into(),
            src: None,
            credit: Some(credit),
        })
        .await
        .map_err(|e| SendError::Disconnected(e.to_string()))?;
    Ok(Arc::new(read_manifest(link).await?))
}

/// The opener side of a download, data phase: the map is answered, then `run`.
pub async fn download_run(
    link: &mut JobLink,
    manifest: Arc<Manifest>,
    need: Option<Need>,
    sink: Arc<dyn Sink>,
    o: RecvOptions,
) -> Result<RecvReport, SendError> {
    run(link, manifest, need, sink, o).await
}

/// One download from open to report: `download_open` then `download_run(None)`.
pub async fn download_job(
    link: &mut JobLink,
    src_root: &str,
    flags: u32,
    sink: Arc<dyn Sink>,
    mut o: RecvOptions,
) -> Result<RecvReport, SendError> {
    let credit = o.credit;
    let t0 = Instant::now();
    let m = download_open(link, src_root, flags, credit).await?;
    if std::env::var_os("PS5UPLOAD_AVA1_TIMING").is_some() {
        let _ = writeln!(
            std::io::stderr(),
            "ava1 download open (lanes, JobOpen, manifest): {:.0}ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
    o.flags = flags; // ruling 12: `run` reads only `o.flags`
    download_run(link, m, None, sink, o).await
}

/// The responder side of an upload (a host for uploads): the JobOpen already arrived.
/// The ack carries the job's absolute grant (the extra credit note: `Credit` frames are
/// incremental; only the ack sets the sender's window).
pub async fn receive_job(
    link: &mut JobLink,
    open: JobOpen,
    sink: Arc<dyn Sink>,
    mut o: RecvOptions,
) -> Result<RecvReport, SendError> {
    o.flags = open.flags; // ruling 12 / Q4: the journal's Open records the job's flags
    link.control
        .send(&JobOpenAck {
            job_id: link.job_id,
            status: gen::STATUS_OK,
            credit: o.credit,
            staged: 0,
            workers: 4,
            message: None,
        })
        .await
        .map_err(|e| SendError::Disconnected(e.to_string()))?;
    let m = match read_manifest(link).await {
        Ok(m) => m,
        Err(e) => {
            // The ack is already out; the sender learns about a bad manifest through the
            // JobDone (its open loop reads both), not a second ack.
            let status = match &e {
                SendError::Refused { status, .. } => *status,
                _ => gen::ERR_PROTOCOL,
            };
            let _ = link
                .control
                .send(&JobDone {
                    job_id: link.job_id,
                    status,
                    files: 0,
                    bytes: 0,
                    message: Some(e.to_string()),
                    settling: None,
                })
                .await;
            return Err(e);
        }
    };
    run(link, Arc::new(m), None, sink, o).await
}

/// The responder side of a `Resume` (SPEC.md §11.5): the stored manifest is already in hand, so
/// there is no ack or manifest exchange — the job's `JobMap` goes out and the job continues
/// exactly as a `JobOpen` resume would (journal replay, §13.4 re-check, map, data).
pub async fn resume_job(
    link: &mut JobLink,
    manifest: Manifest,
    sink: Arc<dyn Sink>,
    o: RecvOptions,
) -> Result<RecvReport, SendError> {
    // No ack carries the grant on this path, so it goes out as a `Credit` (SPEC.md §11.5).
    link.control
        .send(&Credit {
            job_id: link.job_id,
            bytes: o.credit,
        })
        .await
        .map_err(|e| SendError::Disconnected(e.to_string()))?;
    run(link, Arc::new(manifest), None, sink, o).await
}

struct Large {
    /// The group CVs, shared with the sync batch: the batch syncs the very instance the
    /// loop puts CVs into (the outboard's shadow-rename model forbids a second instance
    /// of the same path), so puts and the batch's sync serialize behind this lock.
    hasher_cvs: Option<Arc<Mutex<Outboard>>>,
    written: RangeSet,
    durable: RangeSet,
    root: Option<[u8; 32]>,
    /// A relay's root of a one-group file, hashed from the chunk as it passed through (nothing
    /// is kept to read back).
    single: Option<[u8; 32]>,
}

/// The outboard for a file of `groups` groups. Running out of descriptors is an error to
/// retry (it must not look like a missing outboard, which fails the commit for good); any other
/// failure leaves the file without one, as before.
fn open_outboard(path: &std::path::Path, groups: u64) -> Result<Option<Outboard>, SendError> {
    match Outboard::open(path, groups) {
        Ok(ob) => Ok(Some(ob)),
        Err(e) if is_fd_exhausted(&e) => Err(SendError::Source(e)),
        Err(_) => Ok(None),
    }
}

fn new_large(dir: &std::path::Path, m: &Manifest, id: u32) -> Result<Large, SendError> {
    let size = m
        .entry(id)
        .expect("an id the receiver validated against the manifest")
        .size;
    let hasher_cvs = if verify::groups(size) >= 2 {
        open_outboard(&dir.join(format!("{id}.ob")), verify::groups(size))?
            .map(|ob| Arc::new(Mutex::new(ob)))
    } else {
        None
    };
    Ok(Large {
        hasher_cvs,
        written: RangeSet::new(),
        durable: RangeSet::new(),
        root: None,
        single: None,
    })
}

/// A chunk of `len` bytes at `off` of a file of `size` must be group-aligned, inside the
/// file, and whole groups unless it ends the file.
fn check_chunk_range(size: u64, off: u64, len: u64) -> Result<(), SendError> {
    // An empty chunk carries nothing and describes nothing (an empty file has no chunks):
    // refused, or a peer could buffer one per aligned offset for free.
    if len == 0 && size > 0 {
        return Err(SendError::Protocol("an empty chunk".into()));
    }
    if !off.is_multiple_of(GROUP)
        || off > size
        || len > size - off
        || (!len.is_multiple_of(GROUP) && off + len != size)
    {
        return Err(SendError::Protocol("a chunk outside its file".into()));
    }
    Ok(())
}

/// What the ordered receiver does with a data frame about to enter its reorder buffer.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Keep,
    /// Behind the cursor or already buffered: the bytes are of no use (credit still returns).
    Drop,
}

/// The most the ordered reorder buffer may hold, in credit windows. The sender writes in order,
/// so what waits here is what arrived ahead of a slower lane's frame; a peer that keeps sending
/// ahead of a frame it never sends would otherwise grow it without end (the credit is returned
/// on receipt).
const REORDER_WINDOWS: u64 = 4;

/// What one buffered entry costs against the cap: its bytes plus a fixed overhead for the map
/// node, so entries cannot be free whatever their size.
fn held_cost(len: u64) -> u64 {
    len.saturating_add(64)
}

/// Validates the key of a data frame before it is buffered (final review: engine #4): the file
/// must be a file of the manifest, the range inside it, the key at or past the cursor and not
/// already held, and the buffer within `REORDER_WINDOWS` of credit.
#[allow(clippy::too_many_arguments)]
fn admit_ordered(
    m: &Manifest,
    cursor: (u32, u64),
    held: &BTreeMap<(u32, u64), (bool, Vec<u8>)>,
    held_bytes: u64,
    credit: u64,
    file_id: u32,
    off: u64,
    len: u64,
    whole: bool,
) -> Result<Admit, SendError> {
    let e = m
        .entry(file_id)
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .ok_or_else(|| {
            SendError::Protocol(format!("a frame names {file_id}, which is not a file here"))
        })?;
    if whole {
        if off != 0 || len != e.size {
            return Err(SendError::Protocol(
                "a bundled file of the wrong size".into(),
            ));
        }
    } else {
        check_chunk_range(e.size, off, len)?;
    }
    if (file_id, off) < cursor || held.contains_key(&(file_id, off)) {
        return Ok(Admit::Drop);
    }
    if held_bytes.saturating_add(held_cost(len)) > credit.saturating_mul(REORDER_WINDOWS) {
        return Err(SendError::Protocol(
            "the ordered sender ran too far ahead of a frame it has not sent".into(),
        ));
    }
    Ok(Admit::Keep)
}

/// A chunk (SPEC.md §12.2, the C receiver's ava1_apply_chunk): inside the file, group
/// aligned, whole groups unless it ends the file. A wire-supplied id the manifest does
/// not carry is a protocol error — never a panic (ruling 3).
async fn apply_chunk(
    sink: &Arc<dyn Sink>,
    m: &Arc<Manifest>,
    dir: &std::path::Path,
    large: &mut HashMap<u32, Large>,
    id: u32,
    off: u64,
    data: Vec<u8>,
) -> Result<(), SendError> {
    let Some(e) = m.entry(id) else {
        return Err(SendError::Protocol(format!(
            "a chunk names file {id}, which this manifest has none of"
        )));
    };
    if e.kind != gen::ENTRY_FILE {
        return Err(SendError::Protocol(format!(
            "a chunk names {id}, which is not a file"
        )));
    }
    let size = e.size;
    let len = data.len() as u64;
    check_chunk_range(size, off, len)?;
    if let std::collections::hash_map::Entry::Vacant(v) = large.entry(id) {
        v.insert(new_large(dir, m, id)?);
    }
    let l = large.get_mut(&id).expect("inserted above");
    let s2 = sink.clone();
    let data = Arc::new(data);
    let d2 = data.clone();
    tokio::task::spawn_blocking(move || s2.write_at(id, off, &d2))
        .await
        .map_err(proto)??;
    // The CVs are hashed before the outboard's lock is taken (BLAKE3 is the slow part);
    // the batch task's sync holds the lock only for the shadow rename, never for a hash.
    let mut cvs = Vec::with_capacity(data.len().div_ceil(GROUP as usize));
    for (k, g) in data.chunks(GROUP as usize).enumerate() {
        let gi = off / GROUP + k as u64;
        cvs.push((gi, verify::group_cv(g, gi)));
    }
    if let Some(ob) = l.hasher_cvs.as_ref() {
        let mut ob = ob.lock().unwrap();
        for (gi, cv) in cvs {
            ob.put(gi, &cv)?;
        }
    }
    if sink.transient_relay() && verify::groups(size) < 2 && off == 0 && len == size {
        l.single = Some(*blake3::hash(&data).as_bytes());
    }
    l.written.insert(off, off + len);
    Ok(())
}

/// What a relay holds as the root of a finished file, to compare with the root the source
/// announced: the root of the chunks that passed through it (the CVs of a file of several
/// groups, the hash of a one-group file) or, when part of the file never passed through here
/// (the destination already had it), the announced root itself, which the destination checks
/// over the complete file.
fn relay_root(
    groups: u64,
    cvs: Option<Vec<[u8; 32]>>,
    single: Option<[u8; 32]>,
    announced: [u8; 32],
) -> [u8; 32] {
    if groups >= 2 {
        cvs.map_or(announced, |c| verify::root_from_cvs(&c))
    } else {
        single.unwrap_or(announced)
    }
}

/// From here on both sides are identical: journal, map, apply, sync, commit, finish
/// (SPEC.md §12.6, §13, §14).
///
/// The loop runs in `run_loop`; this wrapper joins the sync batch on every exit path.
/// A dropped handle detaches the task, and a detached batch keeps appending to the
/// job's journal after the job is gone — two writers against the journal's
/// single-writer directory if the peer reopens the same job id (ruling 19). Joining
/// cannot deadlock: the batch awaits only its own `spawn_blocking` I/O and the
/// control outbox, whose sends fail as soon as the session's writer task ends
/// (bounded by `dead_after`), never anything the run loop holds.
async fn run(
    link: &mut JobLink,
    m: Arc<Manifest>,
    need_hint: Option<Need>,
    sink: Arc<dyn Sink>,
    o: RecvOptions,
) -> Result<RecvReport, SendError> {
    let mut batch_handle: Option<tokio::task::JoinHandle<Result<BatchDone, SendError>>> = None;
    let outcome = run_loop(link, m, need_hint, sink, o, &mut batch_handle).await;
    if let Some(h) = batch_handle.take() {
        let _ = h.await;
    }
    outcome
}

/// Sends the receiver's map (what it already has) to the sender.
async fn send_need(link: &mut JobLink, job_id: [u8; 16], need: &Need) -> Result<(), SendError> {
    for p in need.to_pages(job_id, gen::STATUS_OK) {
        link.control
            .send(&p)
            .await
            .map_err(|e| SendError::Disconnected(e.to_string()))?;
    }
    Ok(())
}

async fn run_loop(
    link: &mut JobLink,
    m: Arc<Manifest>,
    need_hint: Option<Need>,
    sink: Arc<dyn Sink>,
    o: RecvOptions,
    batch_handle: &mut Option<tokio::task::JoinHandle<Result<BatchDone, SendError>>>,
) -> Result<RecvReport, SendError> {
    let job_id = link.job_id;
    let mut tm = StageTimes::new();
    let dir = journal::job_dir(&o.jobs_dir, &job_id);
    std::fs::create_dir_all(&dir)?;
    // The destination root and the staging decision go into the journal's Open, exactly as
    // the C receiver records them (ava1_recv.c:380-430 decides, :1026-1040 writes), so
    // either side can resume a job the other started. The sink supplies both (ruling Q1).
    let (dest_root, staged) = sink.resume_key().unwrap_or((String::new(), false));
    let fresh = JnlOpen {
        job_id,
        manifest_hash: m.hash(),
        kind: gen::JOB_DOWNLOAD,
        flags: o.flags,
        staged: u8::from(staged),
        root: dest_root.clone(),
    };
    let mut st = State::default();
    // A fresh job (no journal to replay, no relay hint) needs nothing from the sender's
    // map, so the (empty) map goes out BEFORE the journal and the sink's directories are
    // made durable: the sender's turnaround to its first data frame then overlaps that
    // setup (several drive-cache flushes on a Mac) instead of waiting behind it. Data that
    // arrives meanwhile waits in the inbox, which the credit window already bounds.
    let early_map = need_hint.is_none() && !sink.transient_relay();
    let mut need_sent = false;
    let (mut jnl, open_rec) = match Journal::open(&dir) {
        Ok((j, recs)) => {
            for r in &recs {
                st.apply(r);
            }
            match st.open.clone().filter(|jo| {
                jo.kind == gen::JOB_DOWNLOAD
                    && jo.flags == o.flags
                    && jo.root == dest_root
                    && jo.manifest_hash == m.hash()
            }) {
                Some(jo) => (j, jo), // resume: replay + the recorded Open
                None => {
                    drop(j); // one writer per directory (ruling 19): a different job
                    st = State::default(); // start over
                    if early_map {
                        send_need(link, job_id, &Need::default()).await?;
                        need_sent = true;
                    }
                    journal::write_manifest(&dir, &m)?;
                    let j = Journal::create(&dir, &fresh)?;
                    st.apply(&Record::Open(fresh.clone()));
                    (j, fresh.clone())
                }
            }
        }
        Err(_) => {
            if early_map {
                send_need(link, job_id, &Need::default()).await?;
                need_sent = true;
            }
            journal::write_manifest(&dir, &m)?;
            let j = Journal::create(&dir, &fresh)?;
            st.apply(&Record::Open(fresh.clone()));
            (j, fresh.clone())
        }
    };
    sink.enable_log(&dir);
    let s2 = sink.clone();
    let m2 = m.clone();
    tokio::task::spawn_blocking(move || s2.prepare(&m2))
        .await
        .map_err(proto)??;
    // Durable-by-log recovery (SPEC.md §15.7): files the journal calls done but not yet swept are made
    // again from the pack where a crash lost them and swept; the ones whose record is gone are reset
    // (and so resent). Before the map, so the sender is told the truth.
    {
        let (s2, st2) = (sink.clone(), st.clone());
        let lost = tokio::task::spawn_blocking(move || s2.recover_log(&st2))
            .await
            .map_err(proto)??;
        for id in lost {
            let rec = Record::Reset(id);
            st.apply(&rec);
            jnl.append(&rec)?;
        }
        let (j, s) = sweep_logged(&sink, jnl, st, true).await?;
        jnl = j;
        st = s;
        sink.log_cleanup();
    }

    if sink.transient_relay() {
        if let Some(hint) = &need_hint {
            st.done = hint.done.clone();
            st.ranges = hint.partial.clone();
        }
    }

    // Resume check (SPEC.md §13.4): every durable group of every partial file is re-hashed
    // against the sink and the outboard; a mismatch resets the file before the map.
    let mut large: HashMap<u32, Large> = HashMap::new();
    for (id, r) in st.ranges.clone() {
        let Some(e) = m.entry(id) else {
            // A journal id the manifest does not carry (corrupt state, or a hash
            // collision): an error ends the job, never a panic on the job task (ruling
            // 3 covers wire ids; this is the journal's).
            return Err(SendError::Protocol(format!(
                "the journal names file {id}, which this manifest has none of"
            )));
        };
        let size = e.size;
        let mut ob = open_outboard(&dir.join(format!("{id}.ob")), verify::groups(size))?;
        let mut good = RangeSet::new();
        for (s, e) in r.iter() {
            if sink.transient_relay() {
                good.insert(s, e);
                continue;
            }
            let mut g = s / GROUP;
            while g * GROUP < e {
                let len = (size - g * GROUP).min(GROUP) as usize;
                let mut buf = vec![0u8; len];
                let ok = sink.read_at(id, g * GROUP, &mut buf).is_ok()
                    && (verify::groups(size) < 2
                        || ob.as_ref().and_then(|o| o.get(g)) == Some(verify::group_cv(&buf, g)));
                if ok {
                    good.insert(g * GROUP, g * GROUP + len as u64);
                }
                g += 1;
            }
        }
        if good.covered() != r.covered() {
            let rec = Record::Reset(id);
            st.apply(&rec);
            jnl.append(&rec)?;
            ob = open_outboard(&dir.join(format!("{id}.ob")), verify::groups(size))?;
            good = RangeSet::new();
        }
        st.ranges.insert(id, good.clone());
        large.insert(
            id,
            Large {
                hasher_cvs: ob.map(|o| Arc::new(Mutex::new(o))),
                written: RangeSet::new(),
                durable: good,
                root: st.roots.get(&id).copied(),
                single: None,
            },
        );
    }
    // The sink cuts its stream back to the journal's state (the Stored zip's resume); a
    // sink that cannot gets a fresh job.
    if !sink.transient_relay() {
        let partial_of = |st: &State| -> BTreeMap<u32, RangeSet> {
            st.ranges
                .iter()
                .filter(|(id, rs)| !st.done.contains(id) && rs.covered() > 0)
                .map(|(id, rs)| (*id, rs.clone()))
                .collect()
        };
        let (s2, d2, p2) = (sink.clone(), st.done.clone(), partial_of(&st));
        let r = tokio::task::spawn_blocking(move || s2.position(&d2, &p2))
            .await
            .map_err(proto)?;
        if let Err(why) = r {
            let _ = writeln!(
                std::io::stderr(),
                "ava1: the sink cannot resume this job ({why}); starting it over"
            );
            jnl = Journal::create(&dir, &open_rec)?;
            st = State::default();
            st.apply(&Record::Open(open_rec.clone()));
            large.clear();
            let s2 = sink.clone();
            tokio::task::spawn_blocking(move || s2.position(&BTreeSet::new(), &BTreeMap::new()))
                .await
                .map_err(proto)??;
        }
    }
    // What the job still needs: the relay's hint when given (Task 24), else the replayed
    // state after the resume check (ruling 4: build it here; never change journal.rs).
    let need = match need_hint.clone() {
        Some(n) => n,
        None => Need {
            done: st.done.clone(),
            partial: st
                .ranges
                .iter()
                .filter(|(id, _)| !st.done.contains(id))
                .map(|(id, rs)| (*id, rs.clone()))
                .collect(),
        },
    };
    // An ordered job's cursor must step over what is already durable: the relay's hint,
    // or the partial files of a resume (no frame will arrive for those bytes).
    let ordered_skip = if sink.transient_relay() {
        need_hint
    } else if o.ordered && !need.partial.is_empty() {
        Some(need.clone())
    } else {
        None
    };
    if !need_sent {
        send_need(link, job_id, &need).await?;
    }
    tm.setup_done();
    // Progress: the totals, and the durable counters seeded from the replay so a resumed
    // run reports what the journal already knows (preflight row 21; Task 18's resume test
    // asserts bytes_durable on the resumed run).
    let pg = o.progress.clone();
    pg.bytes_total.store(m.bytes(), Ordering::Relaxed);
    pg.files_total.store(m.files() as u64, Ordering::Relaxed);
    pg.files_durable
        .store(st.done.len() as u64, Ordering::Relaxed);
    let mut durable_bytes = 0u64;
    for id in &st.done {
        let Some(e) = m.entry(*id) else {
            return Err(SendError::Protocol(format!(
                "the journal names file {id}, which this manifest has none of"
            )));
        };
        durable_bytes += e.size;
    }
    pg.bytes_durable.store(
        durable_bytes + st.ranges.values().map(|r| r.covered()).sum::<u64>(),
        Ordering::Relaxed,
    );

    let total_files = m.files() as usize;
    let mut done: BTreeSet<u32> = st.done.clone();
    let mut pending_small: Vec<u32> = Vec::new();
    // A zero-byte file is already complete: its zero range yields no chunks and, in an
    // ordered download, no bundle either (the ordered sender sends every file as chunks),
    // so no frame will ever arrive to mark it done — and the ordered cursor would stall
    // on it forever, holding every later file up. Create the empty file up front and let
    // the first batch journal it durable (SPEC.md §12.6: done always follows Durable), so
    // it completes on its own whether or not anything arrives for it.
    let zero: BTreeSet<u32> = m
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == gen::ENTRY_FILE && e.size == 0)
        .map(|(i, _)| i as u32)
        .collect();
    for id in &zero {
        if done.contains(id) {
            continue; // a resume: the journal already made it durable
        }
        let (s2, id) = (sink.clone(), *id);
        tokio::task::spawn_blocking(move || s2.write_whole(id, &[]))
            .await
            .map_err(proto)??;
        pending_small.push(id);
    }
    let granted = o.credit;
    let mut credit_back = 0u64;
    // The credit still outstanding, exactly as the C receiver counts it (w_avail):
    // the grant, minus every frame received, plus every Credit frame sent back — the
    // sender's in-flight mirror. A job receiving more than `granted` in total is fine
    // (every returned Credit re-opens the window); only more than `granted` at once is
    // not (SPEC.md §12.4, ledger row 19).
    let mut outstanding = granted;
    let mut last_batch = Instant::now();
    // The last moment the job moved: a data frame, a root, a finished write or batch.
    let mut last_progress = Instant::now();
    // A resume (the journal holds finished or partial files) may leave the sender hashing or
    // skipping what is durable for a long while without a frame, so it waits far longer.
    let progress_deadline = progress_limit(
        o.progress_deadline,
        !need.done.is_empty() || !need.partial.is_empty(),
    );
    // Whether the batch in flight had anything to sync (an empty one runs every SYNC_EVERY).
    let mut batch_worked = false;
    // Out-of-order frames wait here. The credit returns on receipt, so the window does not bound
    // them: `admit_ordered` refuses keys outside the manifest and the cursor, and caps the buffer
    // at `REORDER_WINDOWS` windows of the granted credit. `whole` says the entry is a
    // root-checked bundle record rather than a chunk.
    let mut reorder: BTreeMap<(u32, u64), (bool, Vec<u8>)> = BTreeMap::new();
    let mut cursor: (u32, u64) = (0, 0);
    let mut reorder_bytes = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The journal and the applied state move into each sync batch and come back with it;
    // while a batch runs the loop never touches them, and the finish paths below only run
    // when no batch is in flight.
    let mut jnl = Some(jnl);
    let mut st = Some(st);
    // Small files inside the batch that is running (not yet in `done`).
    let mut inflight_small = 0usize;
    // Bundle writes in flight (unordered downloads): at most WRITE_PAR touch the disk at once.
    let mut writes: tokio::task::JoinSet<Result<BundleWritten, SendError>> =
        tokio::task::JoinSet::new();
    let write_gate = Arc::new(tokio::sync::Semaphore::new(WRITE_PAR));
    loop {
        if o.cancel.load(Ordering::Relaxed) {
            let _ = link
                .control
                .send(&JobCancel {
                    job_id,
                    reason: gen::ERR_CANCELLED,
                })
                .await;
            return Err(SendError::Cancelled);
        }
        // A job task never stops reading (ruling 11): the inbox is drained continuously and
        // disk work runs on spawn_blocking — including the sync batch, which runs on a task
        // of its own (`batch_handle`) while this loop keeps routing frames and answering the
        // peer; awaiting room on the control outbox is the backpressure, so a peer that
        // stops reading slows this loop — never a queue here.
        let ev = tokio::select! {
            ev = next(link) => Some(ev?),
            _ = tick.tick() => None,
            w = writes.join_next(), if !writes.is_empty() => {
                let w = w.expect("guarded by `!is_empty`").map_err(proto)??;
                tm.write_time += w.took;
                last_progress = Instant::now();
                credit_back += w.credit;
                for id in w.ok {
                    pending_small.push(id);
                }
                for (id, why) in w.retry {
                    link.control
                        .send(&gen::FileRetry { job_id, file_id: id, reason: why })
                        .await
                        .map_err(|e| SendError::Disconnected(e.to_string()))?;
                }
                None
            }
            joined = join_batch(batch_handle), if batch_handle.is_some() => {
                let b = joined.expect("guarded by `is_some`");
                *batch_handle = None;
                tm.batch_done();
                match b {
                    Ok(out) => {
                        if batch_is_progress(batch_worked) {
                            last_progress = Instant::now();
                        }
                        fold_batch(&mut done, &mut large, &pg, &m, &out);
                        inflight_small = 0;
                        jnl = Some(out.jnl);
                        st = Some(out.st);
                    }
                    Err(e) => return Err(e),
                }
                // Fall through (as an idle turn): the batch check below may start the next
                // batch, or finish the job, right now instead of on the next 50 ms tick.
                None
            }
        };
        // Ruling 2: `ev` is matched below and read by `is_none` — bind first.
        let idle = ev.is_none();
        match ev {
            Some(Inbound::Lane { lane, frame }) => {
                let len = frame.body.len() as u64;
                tm.frame();
                // SPEC.md §12.4 (ledger row 19): a frame that exceeds the credit this job
                // still has outstanding is refused with ERR_CREDIT on that lane; nothing of
                // it is buffered or acknowledged. `outstanding` = granted − received +
                // returned, the sender's in-flight mirror (the C receiver's w_avail).
                if len > outstanding {
                    if let Some(l) = link.lane(lane) {
                        // The sealed Error on the offending lane (SPEC.md §12.4): the peer's
                        // link closes that lane when it reads it, which the sender observes
                        // exactly as the C receiver's explicit close: its lane dies.
                        let _ =
                            l.tx.send(&gen::Error {
                                code: gen::ERR_CREDIT,
                                message: "a frame larger than the credit granted".into(),
                            })
                            .await;
                    }
                    // Ruling: lane only. The sealed Error ends that lane when the peer reads
                    // it (SPEC.md §12.4); the session and the job stay, other lanes go on,
                    // and the refused frame is requeued by the sender as for any dead lane.
                    continue;
                }
                link.control
                    .send(&Received {
                        job_id,
                        lane,
                        seq: frame.channel,
                    })
                    .await
                    .map_err(|e| SendError::Disconnected(e.to_string()))?;
                outstanding -= len;
                last_progress = Instant::now();

                match frame.ty {
                    Chunk::TYPE => {
                        let c: Chunk = frame.decode().map_err(proto)?;
                        if done.contains(&c.file_id) {
                            credit_back += len;
                            continue;
                        }
                        if o.ordered {
                            if admit_ordered(
                                &m,
                                cursor,
                                &reorder,
                                reorder_bytes,
                                granted,
                                c.file_id,
                                c.offset,
                                c.data.len() as u64,
                                false,
                            )? == Admit::Keep
                            {
                                reorder_bytes += held_cost(c.data.len() as u64);
                                reorder.insert((c.file_id, c.offset), (false, c.data));
                            }
                        } else {
                            apply_chunk(&sink, &m, &dir, &mut large, c.file_id, c.offset, c.data)
                                .await?;
                        }
                    }
                    Bundle::TYPE => {
                        let b: Bundle = frame.decode().map_err(proto)?;
                        let mut jobs: Vec<(u32, [u8; 32], Vec<u8>)> = Vec::new();
                        for r in b.records {
                            let Some(e) = m.entry(r.file_id) else {
                                return Err(SendError::Protocol(format!(
                                    "a bundle names file {}, which this manifest has none of",
                                    r.file_id
                                )));
                            };
                            if e.kind != gen::ENTRY_FILE {
                                return Err(SendError::Protocol(format!(
                                    "a bundle names {id}, which is not a file",
                                    id = r.file_id
                                )));
                            }
                            if done.contains(&r.file_id) {
                                continue;
                            }
                            // The C receiver's apply_record: a size mismatch is a file that
                            // changed under the sender (it re-reads), a root mismatch a bad
                            // transfer — FileRetry, never a silent skip that would stall the
                            // job with the file never marked done.
                            if r.data.len() as u64 != e.size {
                                link.control
                                    .send(&gen::FileRetry {
                                        job_id,
                                        file_id: r.file_id,
                                        reason: gen::RETRY_CHANGED,
                                    })
                                    .await
                                    .map_err(|e| SendError::Disconnected(e.to_string()))?;
                                continue;
                            }
                            if o.ordered {
                                if *blake3::hash(&r.data).as_bytes() != r.root {
                                    link.control
                                        .send(&gen::FileRetry {
                                            job_id,
                                            file_id: r.file_id,
                                            reason: gen::RETRY_VERIFY,
                                        })
                                        .await
                                        .map_err(|e| SendError::Disconnected(e.to_string()))?;
                                    continue;
                                }
                                if admit_ordered(
                                    &m,
                                    cursor,
                                    &reorder,
                                    reorder_bytes,
                                    granted,
                                    r.file_id,
                                    0,
                                    r.data.len() as u64,
                                    true,
                                )? == Admit::Keep
                                {
                                    reorder_bytes += held_cost(r.data.len() as u64);
                                    reorder.insert((r.file_id, 0), (true, r.data));
                                }
                            } else {
                                jobs.push((r.file_id, r.root, r.data));
                            }
                        }
                        if !jobs.is_empty() {
                            // Hash-check and write the whole bundle on a blocking task of its
                            // own, concurrently with the bundles before it: one hop per bundle
                            // instead of per file, and the loop goes on draining the inbox.
                            // The bundle's credit returns when its files are on disk (the
                            // backpressure the window exists for), not when it arrived.
                            let (s2, gate) = (sink.clone(), write_gate.clone());
                            writes.spawn(async move {
                                let _permit = gate.acquire_owned().await.map_err(proto)?;
                                tokio::task::spawn_blocking(move || write_bundle(&*s2, jobs, len))
                                    .await
                                    .map_err(proto)?
                            });
                            continue;
                        }
                    }
                    _ => {}
                }
                credit_back += len;
                if o.ordered {
                    // Feed the sink strictly in (file, offset) order; skip files already
                    // done — and zero-byte files, which no frame will ever describe: the
                    // cursor must not wait for a chunk that cannot arrive.
                    loop {
                        while (cursor.0 as usize) < m.entries.len()
                            && (m
                                .entry(cursor.0)
                                .expect("the cursor is inside the manifest")
                                .kind
                                != gen::ENTRY_FILE
                                || done.contains(&cursor.0)
                                || zero.contains(&cursor.0))
                        {
                            cursor = (cursor.0 + 1, 0);
                        }
                        // A relay's source sender omits groups B already has and
                        // whose CV the engine retained. Advance over those gaps;
                        // otherwise the ordered cursor waits forever at offset 0
                        // while later chunks accumulate in `reorder`.
                        if let Some(end) = ordered_skip
                            .as_ref()
                            .and_then(|skip| skip.partial.get(&cursor.0))
                            .and_then(|ranges| {
                                ranges
                                    .iter()
                                    .find(|(start, end)| *start <= cursor.1 && cursor.1 < *end)
                            })
                            .map(|(_, end)| end)
                        {
                            let size = m.entry(cursor.0).map_or(0, |e| e.size);
                            cursor = if end >= size {
                                (cursor.0 + 1, 0)
                            } else {
                                (cursor.0, end)
                            };
                            continue;
                        }
                        let Some((whole, data)) = reorder.remove(&cursor) else {
                            break;
                        };
                        reorder_bytes -= held_cost(data.len() as u64);
                        let (fid, off, n) = (cursor.0, cursor.1, data.len() as u64);
                        if whole {
                            // A bundle record: its root was checked when it arrived, so the
                            // whole-file write is sound without the chunk machinery.
                            let s2 = sink.clone();
                            tokio::task::spawn_blocking(move || s2.write_whole(fid, &data))
                                .await
                                .map_err(proto)??;
                            pending_small.push(fid);
                        } else {
                            apply_chunk(&sink, &m, &dir, &mut large, fid, off, data).await?;
                        }
                        cursor = if off + n
                            >= m.entry(fid)
                                .expect("the cursor is inside the manifest")
                                .size
                        {
                            (fid + 1, 0)
                        } else {
                            (fid, off + n)
                        };
                    }
                    // What the cursor has passed without feeding (a skipped partial range) is no
                    // longer wanted: it must not sit in the buffer for the rest of the job.
                    while let Some((&k, _)) = reorder.first_key_value() {
                        if k >= cursor {
                            break;
                        }
                        if let Some((_, (_, d))) = reorder.pop_first() {
                            reorder_bytes -= held_cost(d.len() as u64);
                        }
                    }
                }
            }
            Some(Inbound::Control(f)) => match f.ty {
                FileRoot::TYPE => {
                    let r: FileRoot = f.decode().map_err(proto)?;
                    let Some(e) = m.entry(r.file_id) else {
                        return Err(SendError::Protocol(format!(
                            "a root names file {}, which this manifest has none of",
                            r.file_id
                        )));
                    };
                    if e.kind != gen::ENTRY_FILE {
                        return Err(SendError::Protocol(format!(
                            "a root names {id}, which is not a file",
                            id = r.file_id
                        )));
                    }
                    last_progress = Instant::now();
                    if e.size == 0 {
                        // A zero-length file is complete when created (up front, above); its root
                        // is the empty root and there is nothing to read back or commit.
                        if r.root != *blake3::hash(&[]).as_bytes() {
                            return Err(SendError::Protocol(format!(
                                "file {} is empty but its root is not the empty root",
                                r.file_id
                            )));
                        }
                    } else {
                        if let std::collections::hash_map::Entry::Vacant(v) = large.entry(r.file_id)
                        {
                            v.insert(new_large(&dir, &m, r.file_id)?);
                        }
                        large.get_mut(&r.file_id).expect("inserted above").root = Some(r.root);
                    }
                }
                JobCancel::TYPE => {
                    return Err(SendError::Refused {
                        status: gen::ERR_CANCELLED,
                        message: "the sender cancelled".into(),
                    });
                }
                _ => {}
            },
            Some(Inbound::LaneUp(_)) | Some(Inbound::LaneDown(_)) => {}
            // `next` maps Closed to an error; this arm documents the shape.
            Some(Inbound::Closed(why)) => return Err(SendError::Disconnected(why)),
            None => {}
        }
        // Review 006 #2: the sender owes bytes (a file is neither written nor in a batch),
        // nothing of ours is in flight (no sync batch, no bundle write: a slow drive is not a
        // stall), and nothing has moved for the whole deadline although the link is alive:
        // end the job. The sender sees `ERR_STALLED` and can resume; a wedged source read
        // there is its problem, not a reason to hold this job open forever.
        // No lane up: the sender is not sending yet (or its lanes are being rebuilt), the same
        // as the console, which arms only while a session is attached.
        if idle && link.lanes().is_empty() {
            last_progress = Instant::now();
        }
        if idle
            && last_progress.elapsed() > progress_deadline
            && done.len() + pending_small.len() + inflight_small < total_files
            && batch_handle.is_none()
            && writes.is_empty()
        {
            let why = format!(
                "progress stalled: no file data for {:.1} s while the sender is still connected",
                last_progress.elapsed().as_secs_f64()
            );
            let _ = writeln!(
                std::io::stderr(),
                "ava1: job {}: {why}",
                crate::hex::encode(&job_id)
            );
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                link.control.send(&JobCancel {
                    job_id,
                    reason: gen::ERR_STALLED,
                }),
            )
            .await;
            return Err(SendError::Disconnected(why));
        }
        if credit_back >= 4 << 20 || (credit_back > 0 && idle) {
            link.control
                .send(&Credit {
                    job_id,
                    bytes: credit_back,
                })
                .await
                .map_err(|e| SendError::Disconnected(e.to_string()))?;
            outstanding += credit_back;
            credit_back = 0;
        }
        if batch_handle.is_none() {
            let finished =
                done.len() >= total_files && pending_small.is_empty() && writes.is_empty();
            if finished {
                let jnl0 = jnl.take().expect("a finished job has no batch in flight");
                let st0 = st.take().expect("a finished job has no batch in flight");
                // Every logged file durable in place before the end (and before a staged tree's rename:
                // the sweep addresses files by path); an engine has no thread to settle behind JobDone.
                let (mut jnl, mut st) = sweep_logged(&sink, jnl0, st0, true).await?;
                sink.log_cleanup();
                let s2 = sink.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || s2.finish())
                    .await
                    .map_err(proto)?
                {
                    // A failure after every byte is durable: report it, never ask for a resend.
                    let status = if e.kind() == io::ErrorKind::AlreadyExists {
                        gen::ERR_EXISTS
                    } else {
                        gen::ERR_IO
                    };
                    let rec = Record::Done(status);
                    st.apply(&rec);
                    jnl.append(&rec)?;
                    link.control
                        .send(&JobDone {
                            job_id,
                            status,
                            files: m.files(),
                            bytes: m.bytes(),
                            message: Some(e.to_string()),
                            settling: None,
                        })
                        .await
                        .map_err(|e| SendError::Disconnected(e.to_string()))?;
                    return Err(SendError::Refused {
                        status,
                        message: e.to_string(),
                    });
                }
                let rec = Record::Done(0);
                st.apply(&rec);
                jnl.append(&rec)?;
                link.control
                    .send(&JobDone {
                        job_id,
                        status: gen::STATUS_OK,
                        files: m.files(),
                        bytes: m.bytes(),
                        message: None,
                        settling: None,
                    })
                    .await
                    .map_err(|e| SendError::Disconnected(e.to_string()))?;
                tm.report(m.files());
                return Ok(RecvReport {
                    files: m.files(),
                    bytes: m.bytes(),
                    manifest: m,
                });
            } else if sync_due(
                last_batch.elapsed(),
                // Every file is accounted for (written, or in a batch): nothing more will
                // arrive, so the sync is the only thing left — never wait a tick for it.
                done.len() + pending_small.len() + inflight_small >= total_files
                    && !pending_small.is_empty(),
            ) {
                last_batch = Instant::now();
                inflight_small = pending_small.len();
                batch_worked =
                    !pending_small.is_empty() || large.values().any(|l| l.written.covered() > 0);
                tm.batch_start();
                let snap = snapshot_batch(
                    job_id,
                    link.control.clone(),
                    &sink,
                    &m,
                    jnl.take().expect("no batch in flight"),
                    &open_rec,
                    st.take().expect("no batch in flight"),
                    &pg,
                    &mut pending_small,
                    &mut large,
                )?;
                *batch_handle = Some(tokio::task::spawn(batch_task(snap)));
            }
        }
    }
}

/// A job that makes no progress for this long while the sender owes bytes is stalled: the
/// receiver ends it rather than wait on the byte-level watchdog forever, since a Ping is a
/// byte and a sender that heartbeats with a wedged data pump (a source read stuck on a
/// network share) keeps the link alive indefinitely (review 006 #2). 3 x the SPEC default
/// `dead_after` (12 s): generous, so a slow-but-moving link or drive is never cut.
pub const PROGRESS_DEADLINE: Duration = Duration::from_secs(36);

/// The deadline for a job: the override (tests, tuning) or the default, and a resumed job waits
/// `RESUME_FACTOR` times as long (the default 36 s becomes the 15 minutes below).
pub(crate) fn progress_limit(explicit: Option<Duration>, resumed: bool) -> Duration {
    match (explicit, resumed) {
        (Some(d), true) => d * RESUME_FACTOR,
        (Some(d), false) => d,
        (None, true) => RESUME_PROGRESS_DEADLINE,
        (None, false) => PROGRESS_DEADLINE,
    }
}

/// How much longer a resumed job may go without progress than a fresh one.
const RESUME_FACTOR: u32 = 25;

/// Whether a finished sync batch counts as progress: one that had something to sync did (even
/// if it made nothing durable, e.g. a long sync that ends in retries); the empty batch that runs
/// every `SYNC_EVERY` did not.
pub(crate) fn batch_is_progress(had_work: bool) -> bool {
    had_work
}

/// The same for a resumed job that already holds partial files: the sender may spend a long
/// time hashing the durable groups it will not resend (no data frame is produced for them),
/// so the clock is far more patient there.
pub const RESUME_PROGRESS_DEADLINE: Duration = Duration::from_secs(900);

/// Concurrent bundle writes. The Mac's file creation scales to a few threads; more only
/// adds contention.
const WRITE_PAR: usize = 4;

/// The most large-file commits (fsync + rename) in flight at once, as the console's commit workers.
const COMMIT_PAR: usize = 4;

/// What one bundle's write task hands back to the loop.
struct BundleWritten {
    /// Written (and root-checked) files, awaiting the next sync batch.
    ok: Vec<u32>,
    /// Files whose bytes did not hash to their root: FileRetry, reason.
    retry: Vec<(u32, u16)>,
    /// The bundle frame's credit, returned now that its files are on disk.
    credit: u64,
    took: Duration,
}

fn write_bundle(
    sink: &dyn Sink,
    files: Vec<(u32, [u8; 32], Vec<u8>)>,
    credit: u64,
) -> Result<BundleWritten, SendError> {
    let t = Instant::now();
    let mut out = BundleWritten {
        ok: Vec::with_capacity(files.len()),
        retry: Vec::new(),
        credit,
        took: Duration::ZERO,
    };
    for (id, root, data) in files {
        if *blake3::hash(&data).as_bytes() != root {
            out.retry.push((id, gen::RETRY_VERIFY));
            continue;
        }
        sink.write_whole_root(id, &root, &data)?;
        out.ok.push(id);
    }
    out.took = t.elapsed();
    Ok(out)
}

/// Batch cadence. A batch is due on the regular interval, and at once when every file
/// has arrived (the tail of the job: nothing more will come, so waiting a tick only adds
/// latency). A shorter interval (60-120 ms) measured no better on a console download
/// (wire-bound, ~40 ms final batch either way) and slower on loopback, where the fsyncs
/// contend with the writes for the drive.
fn sync_due(since_last: Duration, all_in: bool) -> bool {
    since_last >= SYNC_EVERY || all_in
}

const SYNC_EVERY: Duration = Duration::from_millis(250);

/// Opt-in stage timers (`PS5UPLOAD_AVA1_TIMING=1`): one stderr line per finished job, so a
/// slow download can be attributed to the data phase, the write loop or the sync tail.
struct StageTimes {
    on: bool,
    start: Instant,
    setup: Option<Duration>,
    first: Option<Instant>,
    last: Option<Instant>,
    batch_at: Option<Instant>,
    batches: u32,
    batch_total: Duration,
    frames: u32,
    write_time: Duration,
}

impl StageTimes {
    fn new() -> Self {
        Self {
            on: std::env::var_os("PS5UPLOAD_AVA1_TIMING").is_some(),
            start: Instant::now(),
            setup: None,
            first: None,
            last: None,
            batch_at: None,
            batches: 0,
            batch_total: Duration::ZERO,
            frames: 0,
            write_time: Duration::ZERO,
        }
    }
    fn setup_done(&mut self) {
        self.setup = Some(self.start.elapsed());
    }
    fn frame(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.frames += 1;
    }
    fn batch_start(&mut self) {
        self.batch_at = Some(Instant::now());
    }
    fn batch_done(&mut self) {
        if let Some(t) = self.batch_at.take() {
            self.batches += 1;
            self.batch_total += t.elapsed();
        }
    }
    fn report(&self, files: u32) {
        if !self.on {
            return;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let first = self.first.map_or(0.0, |t| ms(t - self.start));
        let last = self.last.map_or(0.0, |t| ms(t - self.start));
        let _ = writeln!(
            std::io::stderr(),
            "ava1 recv timing: files={files} setup={:.0}ms frames={} first_frame={first:.0}ms last_frame={last:.0}ms \
             total={:.0}ms tail={:.0}ms batches={} batch_time={:.0}ms write_time={:.0}ms",
            self.setup.map_or(0.0, ms),
            self.frames,
            ms(self.start.elapsed()),
            ms(self.start.elapsed()) - last,
            self.batches,
            ms(self.batch_total),
            ms(self.write_time),
        );
    }
}

/// The work of one sync batch, owned: the snapshot is taken in the run loop (fast, no
/// I/O) so the batch's blocking I/O can run on its own task while the loop keeps draining
/// the inbox and answering control traffic (ruling 11). `jnl` and `st` move with the job
/// and come back in `BatchDone`, so the loop's journal and applied state are never shared.
struct BatchJob {
    job_id: [u8; 16],
    control: ConnTx,
    sink: Arc<dyn Sink>,
    jnl: Journal,
    open_rec: JnlOpen,
    st: State,
    pg: Arc<Progress>,
    /// The small files (whole bundle records) made durable by this batch.
    small: BTreeSet<u32>,
    /// The large files' ranges made durable by this batch.
    ranges: Vec<FileRange>,
    /// The roots to journal (every large file that has one).
    roots: Vec<RootItem>,
    /// One entry per large file with written data: its outboard — the same instance the
    /// loop keeps putting CVs into — and what was durable before this batch, so the commit
    /// check sees the merged coverage without touching the loop's map.
    large: Vec<BatchLarge>,
}

struct BatchLarge {
    id: u32,
    size: u64,
    prior_durable: RangeSet,
    root: Option<[u8; 32]>,
    ob: Option<Arc<Mutex<Outboard>>>,
    single: Option<[u8; 32]>,
}

/// What the batch hands back to the loop, which folds it into the live state.
struct BatchDone {
    jnl: Journal,
    st: State,
    /// Files made durable by the main record (the journal's files runs).
    small: BTreeSet<u32>,
    /// Large files committed (part → final): the fold removes them from `large`.
    committed: Vec<u32>,
    /// Large files whose root mismatched (Reset journaled, FileRetry sent): the fold
    /// removes them so the next chunk starts them fresh, exactly as before.
    reset: Vec<u32>,
    /// The large-file ranges this batch made durable: the fold subtracts each from
    /// `written` — the loop keeps writing while the batch runs, so a whole-file clear (the
    /// old behaviour) would drop the ranges written after the snapshot — and inserts it
    /// into `durable`.
    ranges: Vec<FileRange>,
}

/// Takes the snapshot of the pending work: the pending small files, the large files'
/// written ranges and roots, their outboards, the journal and the applied state. No I/O
/// (ruling 11: the loop must not block), so the loop keeps routing frames while
/// `batch_task` runs.
#[allow(clippy::too_many_arguments)]
fn snapshot_batch(
    job_id: [u8; 16],
    control: ConnTx,
    sink: &Arc<dyn Sink>,
    m: &Arc<Manifest>,
    jnl: Journal,
    open_rec: &JnlOpen,
    st: State,
    pg: &Arc<Progress>,
    small: &mut Vec<u32>,
    large: &mut HashMap<u32, Large>,
) -> Result<BatchJob, SendError> {
    let mut ranges = Vec::new();
    let mut roots = Vec::new();
    let mut entries = Vec::new();
    for (id, l) in large.iter() {
        if let Some(root) = l.root {
            roots.push(RootItem { file_id: *id, root });
        }
        let Some(e) = m.entry(*id) else {
            // `large` holds ids the journal replayed; a bad one is an error, not a panic
            // on the job task.
            return Err(SendError::Protocol(format!(
                "file {id} has written data but this manifest has none of it"
            )));
        };
        // A file whose bytes were all made durable by an earlier batch but whose root
        // arrived only afterwards (the root rides the control connection, the chunks the
        // lanes: either may win) still has to be committed: without this it was skipped
        // forever, the job never finished, and the receiver sat idle (T28: the "hang").
        let awaiting_commit = l.root.is_some() && l.durable.is_full(e.size);
        if l.written.covered() == 0 && !awaiting_commit {
            continue;
        }
        let size = e.size;
        for (s, e) in l.written.iter() {
            ranges.push(FileRange {
                file_id: *id,
                offset: s,
                len: e - s,
            });
        }
        entries.push(BatchLarge {
            id: *id,
            size,
            prior_durable: l.durable.clone(),
            root: l.root,
            ob: l.hasher_cvs.clone(),
            single: l.single,
        });
    }
    Ok(BatchJob {
        job_id,
        control,
        sink: sink.clone(),
        jnl,
        open_rec: open_rec.clone(),
        st,
        pg: pg.clone(),
        small: std::mem::take(small).into_iter().collect(),
        ranges,
        roots,
        large: entries,
    })
}

/// Sync → journal → Durable, then commit the complete large files (SPEC.md §12.6). The
/// durability ordering is unchanged — the sink's bytes, then the outboards, then the
/// journal record, then the Durable frames, then the commits — but it all runs off the
/// run-loop task, so the loop keeps draining its inbox and answering the peer while the
/// fsyncs run (a receiver that goes silent for seconds loses the session: the peer's
/// heartbeats go unanswered, and a sender's credit stalls too). Every appended record is
/// applied to `st` and, once the journal passes COMPACT_AT, it is compacted from exactly
/// the Open record that created it and the applied state (ruling 5).
async fn batch_task(job: BatchJob) -> Result<BatchDone, SendError> {
    let BatchJob {
        job_id,
        control,
        sink,
        jnl,
        open_rec,
        mut st,
        pg,
        small,
        ranges,
        roots,
        large,
    } = job;
    let mut jnl = jnl;
    // Nothing written since the last batch: nothing to sync, journal or send — but logged files that
    // have aged are swept (the batch cadence is the sweep's clock).
    if small.is_empty() && ranges.is_empty() && roots.is_empty() {
        if sink.unswept() > 0 {
            let force = sink.log_pressure();
            (jnl, st) = sweep_logged(&sink, jnl, st, force).await?;
        }
        return Ok(BatchDone {
            jnl,
            st,
            small,
            committed: Vec::new(),
            reset: Vec::new(),
            ranges,
        });
    }
    // The bytes first: the small files and every large file with a new range (the same
    // ids the old inline batch synced).
    let small_ids: Vec<u32> = small.iter().copied().collect();
    let large_ids: Vec<u32> = large.iter().map(|l| l.id).collect();
    let groups: Vec<LoggedGroup> = if !small_ids.is_empty() || !large_ids.is_empty() {
        let s2 = sink.clone();
        tokio::task::spawn_blocking(move || s2.sync_batch(&small_ids, &large_ids))
            .await
            .map_err(proto)??
    } else {
        Vec::new()
    };
    // The outboards: sync the very instance the loop is still putting CVs into (a second
    // instance would rebuild its image from disk and drop the in-flight puts), so the
    // loop's puts serialize with the sync behind the same lock.
    let obs: Vec<Arc<Mutex<Outboard>>> = large.iter().filter_map(|l| l.ob.clone()).collect();
    if !obs.is_empty() {
        tokio::task::spawn_blocking(move || {
            for ob in &obs {
                ob.lock().unwrap().sync()?;
            }
            Ok::<(), io::Error>(())
        })
        .await
        .map_err(proto)??;
    }
    // One JnlBatch, or with the pack log one per segment the batch's records sit in (the first carries
    // the large files' ranges and roots); each names its files and the byte range of their records.
    let recs: Vec<Record> = if groups.is_empty() {
        vec![Record::Batch(JnlBatch {
            files: runs(&small),
            ranges: ranges.clone(),
            roots,
            pack_len: None,
            pack_offset: None,
            pack_segment: None,
        })]
    } else {
        groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                Record::Batch(JnlBatch {
                    files: runs(&g.ids()),
                    ranges: if i == 0 { ranges.clone() } else { Vec::new() },
                    roots: if i == 0 { roots.clone() } else { Vec::new() },
                    pack_segment: Some(g.segment),
                    pack_offset: Some(g.offset),
                    pack_len: Some(g.len),
                })
            })
            .collect()
    };
    let (j, recs, r) = tokio::task::spawn_blocking(move || {
        let r = recs.iter().try_for_each(|rec| jnl.append(rec));
        (jnl, recs, r)
    })
    .await
    .map_err(proto)?;
    jnl = j;
    r?;
    for rec in &recs {
        st.apply(rec);
    }
    sink.batch_journaled(&groups);
    for r in &ranges {
        pg.bytes_durable.fetch_add(r.len, Ordering::Relaxed);
    }
    for chunk in ranges.chunks(2000) {
        control
            .send(&Durable {
                job_id,
                files: Vec::new(),
                ranges: chunk.to_vec(),
            })
            .await
            .map_err(|e| SendError::Disconnected(e.to_string()))?;
    }
    for chunk in runs(&small).chunks(2000) {
        control
            .send(&Durable {
                job_id,
                files: chunk.to_vec(),
                ranges: Vec::new(),
            })
            .await
            .map_err(|e| SendError::Disconnected(e.to_string()))?;
    }
    // Commit the complete large files: the merged coverage is the prior durable state
    // (from the snapshot) plus this batch's ranges — the same view the old inline batch
    // had after applying the record.
    let mut committed = Vec::new();
    let mut verified: Vec<u32> = Vec::new();
    let mut reset = Vec::new();
    for l in &large {
        let Some(root) = l.root else {
            continue;
        };
        let mut durable = l.prior_durable.clone();
        for r in ranges.iter().filter(|r| r.file_id == l.id) {
            durable.insert(r.offset, r.offset + r.len);
        }
        if !durable.is_full(l.size) {
            continue;
        }
        let actual = if sink.transient_relay() {
            // B owns durability and checks the complete root. A's skipped bytes
            // exist only on B, so its transient sink cannot reread them here. What did pass
            // through here is checked against the root the source announced (engine #5).
            let cvs: Option<Vec<[u8; 32]>> = l.ob.as_ref().and_then(|ob| {
                let ob = ob.lock().unwrap();
                (0..verify::groups(l.size)).map(|g| ob.get(g)).collect()
            });
            Some(relay_root(verify::groups(l.size), cvs, l.single, root))
        } else if verify::groups(l.size) >= 2 {
            let ob = l
                .ob
                .as_ref()
                .ok_or_else(|| io::Error::other("the outboard of a multi-group file is missing"))?;
            let cvs: Option<Vec<[u8; 32]>> = {
                let ob = ob.lock().unwrap();
                (0..verify::groups(l.size)).map(|g| ob.get(g)).collect()
            };
            cvs.map(|c| verify::root_from_cvs(&c))
        } else {
            let s2 = sink.clone();
            let (id, size) = (l.id, l.size);
            tokio::task::spawn_blocking(move || one_group_root(s2.as_ref(), id, size))
                .await
                .map_err(proto)?
        };
        if actual != Some(root) {
            let rec = Record::Reset(l.id);
            let (j, rec, r) = tokio::task::spawn_blocking(move || {
                let r = jnl.append(&rec);
                (jnl, rec, r)
            })
            .await
            .map_err(proto)?;
            jnl = j;
            r?;
            st.apply(&rec);
            control
                .send(&gen::FileRetry {
                    job_id,
                    file_id: l.id,
                    reason: gen::RETRY_VERIFY,
                })
                .await
                .map_err(|e| SendError::Disconnected(e.to_string()))?;
            reset.push(l.id);
            continue;
        }
        verified.push(l.id);
    }
    // Commit the verified files, up to COMMIT_PAR at once (each is an fsync + rename the sink runs
    // with no lock held), then make the new names durable, and only then journal them: a file is
    // called done after its data fsync (this batch's sync), its rename, its directory's fsync.
    // Parallelism is between files, never between a file's commit and its own record. A commit
    // that fails ends the job, but only after the others have returned, and the ones that went
    // through are still journaled (their renames are in place; resending them would cost the file).
    let (ok, failed) = commit_verified(&sink, &verified).await;
    if !ok.is_empty() {
        let (s2, ids) = (sink.clone(), ok.clone());
        tokio::task::spawn_blocking(move || s2.sync_committed(&ids))
            .await
            .map_err(proto)??;
    }
    committed.extend(ok);
    // ONE journal record, one drive flush and one Durable for every file this batch
    // committed (T28): per-file records meant a full-drive flush per file, so an ordered
    // download (every file goes through the large path) crawled and, under load, looked hung.
    if !committed.is_empty() {
        let set: BTreeSet<u32> = committed.iter().copied().collect();
        let rec = Record::Batch(JnlBatch {
            files: runs(&set),
            ranges: Vec::new(),
            roots: Vec::new(),
            pack_len: None,
            pack_offset: None,
            pack_segment: None,
        });
        let (j, rec, r) = tokio::task::spawn_blocking(move || {
            let r = jnl.append(&rec);
            (jnl, rec, r)
        })
        .await
        .map_err(proto)?;
        jnl = j;
        r?;
        st.apply(&rec);
        pg.files_durable
            .fetch_add(committed.len() as u64, Ordering::Relaxed);
        for chunk in runs(&set).chunks(2000) {
            control
                .send(&Durable {
                    job_id,
                    files: chunk.to_vec(),
                    ranges: Vec::new(),
                })
                .await
                .map_err(|e| SendError::Disconnected(e.to_string()))?;
        }
    }
    if let Some(e) = failed {
        return Err(e.into());
    }
    // Logged files that have aged become durable in place now (all of them when the log is full).
    if sink.unswept() > 0 {
        let force = sink.log_pressure();
        (jnl, st) = sweep_logged(&sink, jnl, st, force).await?;
    }
    // The journal is compacted once it passes COMPACT_AT (ruling 5): the engine-side
    // journal must not grow without bound, and the C side does the same.
    if jnl.len() > journal::COMPACT_AT {
        let (j, st2, r) = tokio::task::spawn_blocking(move || {
            let r = jnl.compact(&open_rec, &st);
            (jnl, st, r)
        })
        .await
        .map_err(proto)?;
        jnl = j;
        st = st2;
        r?;
    }
    Ok(BatchDone {
        jnl,
        st,
        small,
        committed,
        reset,
        ranges,
    })
}

/// Commits `ids` on the blocking pool, at most `COMMIT_PAR` at a time. Returns the ids that were
/// committed, in `ids` order, and the first error. Never returns while a commit is still running:
/// a failure lets the others finish (a rename cannot be recalled) and reports what went through.
async fn commit_verified(sink: &Arc<dyn Sink>, ids: &[u32]) -> (Vec<u32>, Option<io::Error>) {
    let gate = Arc::new(tokio::sync::Semaphore::new(COMMIT_PAR));
    let mut set = tokio::task::JoinSet::new();
    for (k, &id) in ids.iter().enumerate() {
        let (s2, gate) = (sink.clone(), gate.clone());
        set.spawn(async move {
            let _permit = gate
                .acquire_owned()
                .await
                .expect("the gate is never closed");
            let r = tokio::task::spawn_blocking(move || s2.commit(id))
                .await
                .unwrap_or_else(|e| Err(io::Error::other(format!("commit task died: {e}"))));
            (k, r)
        });
    }
    let mut done: Vec<(usize, u32)> = Vec::new();
    let mut first_err: Option<(usize, io::Error)> = None;
    while let Some(j) = set.join_next().await {
        match j {
            Ok((k, Ok(()))) => done.push((k, ids[k])),
            Ok((k, Err(e))) => {
                if first_err.as_ref().is_none_or(|(fk, _)| k < *fk) {
                    first_err = Some((k, e));
                }
            }
            Err(e) => {
                first_err.get_or_insert((
                    usize::MAX,
                    io::Error::other(format!("commit task died: {e}")),
                ));
            }
        }
    }
    done.sort_unstable();
    (
        done.into_iter().map(|(_, id)| id).collect(),
        first_err.map(|(_, e)| e),
    )
}

/// Makes the logged files that are due (every one when `force`) durable in place and journals the
/// sweep (SPEC.md §15.7): files fsynced and directories synced first, then `JnlSweep`, and only then do
/// the files stop holding their pack segments (I3). With `force`, repeats until none is left.
async fn sweep_logged(
    sink: &Arc<dyn Sink>,
    mut jnl: Journal,
    mut st: State,
    force: bool,
) -> Result<(Journal, State), SendError> {
    loop {
        let s2 = sink.clone();
        let ids = tokio::task::spawn_blocking(move || s2.sweep(force))
            .await
            .map_err(proto)??;
        if ids.is_empty() {
            break;
        }
        let set: BTreeSet<u32> = ids.iter().copied().collect();
        let rec = Record::Sweep(runs(&set));
        let (j, rec, r) = tokio::task::spawn_blocking(move || {
            let r = jnl.append(&rec);
            (jnl, rec, r)
        })
        .await
        .map_err(proto)?;
        jnl = j;
        r?;
        st.apply(&rec);
        sink.sweep_journaled(&ids);
        if !force {
            break;
        }
    }
    Ok((jnl, st))
}

/// Awaits the in-flight batch by reference — the handle stays in the slot, so if the
/// select resolves through another arm the borrow simply ends and the next iteration
/// keeps joining (the guard stays true the whole time); the run loop clears the slot
/// after a join completes. `None` when nothing is running.
async fn join_batch(
    h: &mut Option<tokio::task::JoinHandle<Result<BatchDone, SendError>>>,
) -> Option<Result<BatchDone, SendError>> {
    let t = h.as_mut()?;
    Some(match t.await {
        Ok(r) => r,
        Err(e) => Err(SendError::Disconnected(format!(
            "the sync batch task died: {e}"
        ))),
    })
}

/// Folds a finished batch into the loop's live state. The loop kept writing while the
/// batch ran, so `written` loses only the ranges the batch journaled — never a
/// whole-file clear, which would drop the ranges written after the snapshot.
fn fold_batch(
    done: &mut BTreeSet<u32>,
    large: &mut HashMap<u32, Large>,
    pg: &Progress,
    m: &Manifest,
    b: &BatchDone,
) {
    // The done-insert guard is the one the old inline batch used for the small files: a
    // bundle that re-arrived while the batch ran (the done fold was still pending) can be
    // batched twice, and the duplicate must not count twice.
    for id in &b.small {
        if done.insert(*id) {
            pg.files_durable.fetch_add(1, Ordering::Relaxed);
            pg.bytes_durable.fetch_add(
                m.entry(*id).expect("an id from the manifest").size,
                Ordering::Relaxed,
            );
        }
    }
    for id in &b.committed {
        done.insert(*id);
        large.remove(id);
    }
    for id in &b.reset {
        large.remove(id);
    }
    for r in &b.ranges {
        if let Some(l) = large.get_mut(&r.file_id) {
            l.written = subtract(&l.written, r.offset, r.offset + r.len);
            l.durable.insert(r.offset, r.offset + r.len);
        }
    }
}

/// `set` minus one half-open range (`RangeSet` has only insertion).
fn subtract(set: &RangeSet, from: u64, to: u64) -> RangeSet {
    let mut out = RangeSet::new();
    for (s, e) in set.iter() {
        if e <= from || s >= to {
            out.insert(s, e);
        } else {
            if s < from {
                out.insert(s, from);
            }
            if e > to {
                out.insert(to, e);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- durable-by-log, the engine's sink (SPEC.md §15.7) ----

    fn log_manifest(n: usize) -> Manifest {
        let mut entries = vec![Entry {
            kind: gen::ENTRY_DIR,
            mode: 0o755,
            size: 0,
            mtime: 0,
            path: "d".into(),
            root: None,
        }];
        for i in 0..n {
            entries.push(Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 1,
                path: format!("d/{i}"),
                root: None,
            });
        }
        Manifest { entries }
    }

    fn log_body(i: usize) -> Vec<u8> {
        format!("{i:04}").into_bytes()
    }

    fn log_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let t = std::env::temp_dir().join(format!("ava1-logsink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let jd = t.join("job");
        std::fs::create_dir_all(&jd).unwrap();
        (t, jd)
    }

    fn quick() -> PackOpts {
        PackOpts {
            segment: 512,
            max_unswept: 1 << 20,
            age: Duration::ZERO,
        }
    }

    /// A sink with `n` logged small files written and batched, as the receive loop would.
    fn logged(t: &Path, jd: &Path, n: usize) -> (LocalSink, Vec<LoggedGroup>, State) {
        let m = log_manifest(n);
        let sink = LocalSink::new(t.join("dest"), false).with_log(true, quick());
        sink.enable_log(jd);
        sink.prepare(&m).unwrap();
        for i in 0..n {
            sink.write_whole(i as u32 + 1, &log_body(i)).unwrap();
        }
        let ids: Vec<u32> = (1..=n as u32).collect();
        let groups = sink.sync_batch(&ids, &[]).unwrap();
        assert!(!groups.is_empty(), "the log is on: groups come back");
        sink.batch_journaled(&groups);
        // what the journal would replay to
        let mut st = State::default();
        for g in &groups {
            st.apply(&Record::Batch(JnlBatch {
                files: runs(&g.ids()),
                ranges: vec![],
                roots: vec![],
                pack_segment: Some(g.segment),
                pack_offset: Some(g.offset),
                pack_len: Some(g.len),
            }));
        }
        (sink, groups, st)
    }

    #[test]
    fn the_ordered_reorder_buffer_refuses_keys_outside_the_range_and_is_bounded() {
        let file = |size| Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size,
            mtime: 0,
            path: String::new(),
            root: None,
        };
        let mut a = file(3 * GROUP);
        a.path = "a".into();
        let mut b = file(10);
        b.path = "b".into();
        let m = Manifest {
            entries: vec![a, b],
        };
        let held: BTreeMap<(u32, u64), (bool, Vec<u8>)> =
            BTreeMap::from([((0, GROUP), (false, vec![]))]);
        let cap_of = |credit: u64| credit * REORDER_WINDOWS;
        let g = |cursor, held_bytes, credit, id, off, len, whole| {
            admit_ordered(&m, cursor, &held, held_bytes, credit, id, off, len, whole)
        };
        assert_eq!(
            g((0, 0), 0, GROUP, 0, 0, GROUP, false).unwrap(),
            Admit::Keep
        );
        // Outside the manifest, past the file, misaligned, overrunning: protocol errors.
        assert!(g((0, 0), 0, GROUP, 7, 0, 1, false).is_err());
        assert!(g((0, 0), 0, GROUP, 0, 4 * GROUP, GROUP, false).is_err());
        assert!(g((0, 0), 0, GROUP, 0, 5, GROUP, false).is_err());
        assert!(g((0, 0), 0, GROUP, 0, 2 * GROUP, 2 * GROUP, false).is_err());
        assert!(g((0, 0), 0, GROUP, 1, 0, 11, true).is_err());
        // Behind the cursor, or already held: dropped, not an error.
        assert_eq!(
            g((1, 0), 0, GROUP, 0, 0, GROUP, false).unwrap(),
            Admit::Drop
        );
        assert_eq!(
            g((0, 0), 0, GROUP, 0, GROUP, GROUP, false).unwrap(),
            Admit::Drop
        );
        // Empty chunks are refused, and every entry costs something against the cap.
        assert!(g((0, 0), 0, GROUP, 0, 2 * GROUP, 0, false).is_err());
        assert!(g((0, 0), cap_of(GROUP) - 10, GROUP, 0, 2 * GROUP, 1, false).is_err());
        // The buffer is capped at REORDER_WINDOWS windows of credit.
        let cap = GROUP * REORDER_WINDOWS;
        assert!(g((0, 0), cap - GROUP - 64, GROUP, 0, 2 * GROUP, GROUP, false).is_ok());
        assert!(g((0, 0), cap - GROUP - 63, GROUP, 0, 2 * GROUP, GROUP, false).is_err());
    }

    #[test]
    fn a_relay_compares_the_announced_root_with_the_relayed_bytes() {
        let data: Vec<u8> = (0..(3 * GROUP as usize)).map(|i| (i * 13) as u8).collect();
        let cvs: Vec<[u8; 32]> = data
            .chunks(GROUP as usize)
            .enumerate()
            .map(|(i, g)| verify::group_cv(g, i as u64))
            .collect();
        let real = *blake3::hash(&data).as_bytes();
        assert_eq!(real, verify::root_from_cvs(&cvs));
        let lie = [9u8; 32];
        // Everything passed through: the computed root decides, so a lying announcement differs.
        assert_eq!(relay_root(3, Some(cvs.clone()), None, real), real);
        assert_ne!(relay_root(3, Some(cvs), None, lie), lie);
        // A part never passed through (the destination had it): the announcement stands.
        assert_eq!(relay_root(3, None, None, lie), lie);
        // One group: the hash of the chunk that passed through.
        let one = b"one group of bytes";
        let h = *blake3::hash(one).as_bytes();
        assert_eq!(relay_root(1, None, Some(h), lie), h);
        assert_eq!(relay_root(1, None, None, lie), lie);
    }

    #[test]
    fn many_large_files_written_interleaved_stay_within_the_descriptor_cap() {
        let d = std::env::temp_dir().join(format!("p5a-fdcap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let n = MAX_OPEN as u32 * 2 + 20;
        let size = 3 * GROUP;
        let m = Manifest {
            entries: (0..n)
                .map(|i| Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size,
                    mtime: 0,
                    path: format!("f{i}"),
                    root: None,
                })
                .collect(),
        };
        let body = |i: u32, g: u64| vec![(i as u8).wrapping_add(g as u8 * 40); GROUP as usize];
        // Non-staged (an existing root) and staged (a new one) both.
        for staged in [false, true] {
            let root = d.join(if staged { "new" } else { "old" });
            if !staged {
                std::fs::create_dir_all(&root).unwrap();
            }
            let sink = LocalSink::new(root.clone(), false).with_log(false, PackOpts::default());
            sink.prepare(&m).unwrap();
            // Interleaved: group 0 of every file, then group 1 of every file, ... so every file is
            // evicted from the cache and reopened between its own writes.
            for g in 0..3u64 {
                for i in 0..n {
                    sink.write_at(i, g * GROUP, &body(i, g)).unwrap();
                    assert!(sink.st.lock().unwrap().open.len() <= MAX_OPEN);
                }
                let ids: Vec<u32> = (0..n).collect();
                sink.sync(&ids).unwrap();
            }
            for i in 0..n {
                sink.commit(i).unwrap();
            }
            sink.finish().unwrap();
            for i in 0..n {
                let got = std::fs::read(root.join(format!("f{i}"))).unwrap();
                let want: Vec<u8> = (0..3).flat_map(|g| body(i, g)).collect();
                assert!(got == want, "file {i} (staged: {staged})");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_file_verifies_as_the_empty_root_without_a_read_back() {
        struct NoRead;
        impl Sink for NoRead {
            fn prepare(&self, _: &Manifest) -> io::Result<()> {
                Ok(())
            }
            fn write_at(&self, _: u32, _: u64, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn write_whole(&self, _: u32, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn sync(&self, _: &[u32]) -> io::Result<()> {
                Ok(())
            }
            fn read_at(&self, _: u32, _: u64, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::Unsupported))
            }
            fn commit(&self, _: u32) -> io::Result<()> {
                Ok(())
            }
            fn finish(&self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            one_group_root(&NoRead, 3, 0),
            Some(*blake3::hash(&[]).as_bytes())
        );
        assert_eq!(one_group_root(&NoRead, 3, 10), None);
    }

    #[test]
    fn a_logged_batch_leaves_files_unswept_until_the_sweep_and_drops_every_segment() {
        let (t, jd) = log_dirs("sweep");
        let n = 40;
        let (sink, groups, st) = logged(&t, &jd, n);
        assert!(groups.len() >= 2, "segments of 512 bytes roll: {groups:?}");
        assert_eq!(
            groups.iter().map(|g| g.files.len()).sum::<usize>(),
            n,
            "every file is in exactly one group"
        );
        assert_eq!(st.unswept.len(), n);
        assert_eq!(sink.unswept(), n);
        for i in 0..n {
            assert_eq!(
                std::fs::read(t.join(format!("dest.ava-part/d/{i}"))).unwrap(),
                log_body(i)
            );
        }
        let ids = sink.sweep(true).unwrap();
        assert_eq!(ids.len(), n);
        sink.sweep_journaled(&ids);
        assert_eq!(sink.unswept(), 0);
        sink.log_cleanup();
        let packs = std::fs::read_dir(&jd)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("pack."))
            .count();
        assert_eq!(packs, 0);
    }

    #[test]
    fn a_restart_re_makes_files_lost_with_the_page_cache_from_the_log() {
        let (t, jd) = log_dirs("recover");
        let n = 30;
        let (sink, _groups, st) = logged(&t, &jd, n);
        drop(sink); // the crash: nothing was swept
        for i in 0..n / 2 {
            std::fs::remove_file(t.join(format!("dest.ava-part/d/{i}"))).unwrap();
        }
        std::fs::write(t.join("dest.ava-part/d/20"), b"junk").unwrap();
        let again = LocalSink::new(t.join("dest"), false).with_log(true, quick());
        again.enable_log(&jd);
        again.prepare(&log_manifest(n)).unwrap();
        let lost = again.recover_log(&st).unwrap();
        assert!(lost.is_empty(), "{lost:?}");
        for i in 0..n {
            assert_eq!(
                std::fs::read(t.join(format!("dest.ava-part/d/{i}"))).unwrap(),
                log_body(i),
                "file {i}"
            );
        }
        let ids = again.sweep(true).unwrap();
        assert_eq!(ids.len(), n);
        again.sweep_journaled(&ids);
        again.log_cleanup();
    }

    #[test]
    fn a_torn_tail_or_a_missing_segment_reports_the_files_to_resend() {
        let (t, jd) = log_dirs("torn");
        let n = 12;
        let (sink, groups, st) = logged(&t, &jd, n);
        drop(sink);
        let last = groups.last().unwrap();
        let p = jd.join(format!("pack.{}", last.segment));
        let len = std::fs::metadata(&p).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(len - 3)
            .unwrap();
        let first = &groups[0];
        std::fs::remove_file(jd.join(format!("pack.{}", first.segment))).unwrap();
        let again = LocalSink::new(t.join("dest"), false).with_log(true, quick());
        again.enable_log(&jd);
        again.prepare(&log_manifest(n)).unwrap();
        let lost = again.recover_log(&st).unwrap();
        let mut expect: BTreeSet<u32> = first.ids();
        expect.insert(last.files.last().unwrap().id);
        assert_eq!(lost.into_iter().collect::<BTreeSet<u32>>(), expect);
    }

    #[test]
    fn recovery_runs_whatever_the_log_setting_when_the_replayed_state_has_unswept_files() {
        // a job that crashed with the log on and restarts with it off (the macOS default, or
        // PS5UPLOAD_AVA1_LOG_SMALL=0) must still re-make and sweep its files
        let (t, jd) = log_dirs("logoff");
        let n = 20;
        let (sink, _g, st) = logged(&t, &jd, n);
        drop(sink);
        for i in 0..n / 2 {
            std::fs::remove_file(t.join(format!("dest.ava-part/d/{i}"))).unwrap();
        }
        let off = LocalSink::new(t.join("dest"), false).with_log(false, quick());
        off.enable_log(&jd);
        off.prepare(&log_manifest(n)).unwrap();
        let lost = off.recover_log(&st).unwrap();
        assert!(lost.is_empty(), "{lost:?}");
        for i in 0..n {
            assert_eq!(
                std::fs::read(t.join(format!("dest.ava-part/d/{i}"))).unwrap(),
                log_body(i),
                "file {i}"
            );
        }
        let ids = off.sweep(true).unwrap();
        assert_eq!(ids.len(), n);
        off.sweep_journaled(&ids);
        off.log_cleanup();
        // ... and with the log off, new small files still go the per-file way
        off.write_whole(1, &log_body(0)).unwrap();
        assert_eq!(off.unswept(), 0);
    }

    /// A sink that watches the journal at the moments the durability order matters.
    struct Spy {
        inner: LocalSink,
        jd: PathBuf,
        seen: Mutex<Vec<String>>,
    }

    impl Spy {
        fn replay(&self) -> State {
            let (_, recs) = Journal::open(&self.jd).unwrap();
            let mut st = State::default();
            for r in &recs {
                st.apply(r);
            }
            st
        }
        fn sweeps(&self) -> usize {
            Journal::open(&self.jd)
                .unwrap()
                .1
                .iter()
                .filter(|r| matches!(r, Record::Sweep(_)))
                .count()
        }
        fn packs(&self) -> usize {
            std::fs::read_dir(&self.jd)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("pack."))
                .count()
        }
    }

    impl Sink for Spy {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, d: &[u8]) -> io::Result<()> {
            self.inner.write_at(id, off, d)
        }
        fn write_whole(&self, id: u32, d: &[u8]) -> io::Result<()> {
            self.inner.write_whole(id, d)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, b: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, b)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn unswept(&self) -> usize {
            self.inner.unswept()
        }
        fn sweep(&self, force: bool) -> io::Result<Vec<u32>> {
            let ids = self.inner.sweep(force)?;
            // I2: the files are synced before the record that says so: no JnlSweep is there yet
            self.seen.lock().unwrap().push(format!(
                "sweep returned {} files, {} sweep records so far",
                ids.len(),
                self.sweeps()
            ));
            Ok(ids)
        }
        fn sweep_journaled(&self, ids: &[u32]) {
            // I3: the segments go only after the sweep is durable: the record is there, the packs still are
            self.seen.lock().unwrap().push(format!(
                "journaled {} files: {} sweep records, {} pack files",
                ids.len(),
                self.sweeps(),
                self.packs()
            ));
            let st = self.replay();
            assert!(
                ids.iter().all(|i| !st.unswept.contains(i)),
                "the journal still lists swept files as unswept"
            );
            self.inner.sweep_journaled(ids)
        }
    }

    #[tokio::test]
    async fn the_sweep_is_journaled_after_its_files_and_before_a_segment_is_deleted() {
        let (t, jd) = log_dirs("order");
        let n = 24;
        let (inner, _groups, st0) = logged(&t, &jd, n);
        // a journal holding what the batches would have written
        let open = JnlOpen {
            job_id: [9; 16],
            manifest_hash: [1; 32],
            kind: gen::JOB_DOWNLOAD,
            flags: 0,
            staged: 1,
            root: "x".into(),
        };
        let mut jnl = Journal::create(&jd, &open).unwrap();
        let mut st = State::default();
        st.apply(&Record::Open(open));
        let rec = Record::Snapshot(st0.snapshot());
        jnl.append(&rec).unwrap();
        st.apply(&rec);
        let spy = Arc::new(Spy {
            inner,
            jd: jd.clone(),
            seen: Mutex::default(),
        });
        let sink: Arc<dyn Sink> = spy.clone();
        let (jnl, st) = sweep_logged(&sink, jnl, st, true).await.unwrap();
        assert!(st.unswept.is_empty());
        drop(jnl);
        let seen = spy.seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .any(|l| l.starts_with("sweep returned") && l.ends_with("0 sweep records so far")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|l| l.starts_with("journaled") && l.contains("1 sweep records")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .filter(|l| l.starts_with("journaled"))
                .all(|l| !l.ends_with("0 pack files")),
            "a segment was deleted before its sweep was journaled: {seen:?}"
        );
    }

    #[test]
    fn the_unswept_cap_reports_pressure() {
        let (t, jd) = log_dirs("cap");
        let m = log_manifest(8);
        let sink = LocalSink::new(t.join("dest"), false).with_log(
            true,
            PackOpts {
                segment: 1 << 20,
                max_unswept: 100,
                age: Duration::from_secs(60),
            },
        );
        sink.enable_log(&jd);
        sink.prepare(&m).unwrap();
        assert!(!sink.log_pressure());
        for i in 0..8 {
            sink.write_whole(i as u32 + 1, &log_body(i)).unwrap();
        }
        assert!(sink.log_pressure(), "8 records pass 100 bytes");
        let ids: Vec<u32> = (1..=8).collect();
        let g = sink.sync_batch(&ids, &[]).unwrap();
        sink.batch_journaled(&g);
        assert!(
            sink.sweep(false).unwrap().is_empty(),
            "nothing is old enough yet"
        );
        let swept = sink.sweep(true).unwrap();
        sink.sweep_journaled(&swept);
        assert!(!sink.log_pressure());
    }

    #[cfg(unix)]
    #[test]
    fn directory_syncs_cover_every_directory_and_report_a_failure() {
        let t = std::env::temp_dir().join(format!("ava1-syncdirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let dirs: BTreeSet<PathBuf> = (0..37)
            .map(|i| {
                let d = t.join(format!("d{i:02}"));
                std::fs::create_dir_all(&d).unwrap();
                d
            })
            .collect();
        sync_dirs(&dirs).unwrap();
        let mut one_gone = dirs.clone();
        one_gone.insert(t.join("missing"));
        assert!(
            sync_dirs(&one_gone).is_err(),
            "a directory that cannot be opened is an error"
        );
        let two: BTreeSet<PathBuf> = dirs.iter().take(2).cloned().collect();
        sync_dirs(&two).unwrap();
        let _ = std::fs::remove_dir_all(&t);
    }

    use crate::conn::{FrameReader, FrameWriter};
    use crate::manifest::Entry;
    use crate::router::{ConnTx, Router};
    use crate::session::Timing;
    use tokio::io::{duplex, split};
    use tokio::sync::mpsc;

    /// A `JobLink` over an in-memory pipe whose control frames a drain task consumes, so
    /// the batch's Durable frames never block on a full outbox. The `Link` comes back too:
    /// dropping it aborts the connection's tasks and the outbox's sends would fail.
    fn test_link(job: [u8; 16]) -> (JobLink, crate::link::Link) {
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(1),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let (a, b) = duplex(1 << 20);
        let (ar, aw) = split(a);
        let (tx, mut rx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (link, outbox) =
            crate::link::drive(FrameReader::new(ar), FrameWriter::new(aw), timing, tx);
        tokio::spawn(async move {
            let _keep = b; // the peer half stays open: the writer must not see EOF
            while rx.recv().await.is_some() {}
        });
        (
            JobLink::new(job, Arc::new(Router::default()), ConnTx::new(outbox), None),
            link,
        )
    }

    struct NoopSink;
    impl Sink for NoopSink {
        fn prepare(&self, _m: &Manifest) -> io::Result<()> {
            Ok(())
        }
        fn write_at(&self, _id: u32, _off: u64, _data: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn write_whole(&self, _id: u32, _data: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn sync(&self, _ids: &[u32]) -> io::Result<()> {
            Ok(())
        }
        fn read_at(&self, _id: u32, _off: u64, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn commit(&self, _id: u32) -> io::Result<()> {
            Ok(())
        }
        fn finish(&self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The receiver compacts its journal once it passes COMPACT_AT: the batch is the
    /// caller (ruling 5, preflight row 12), and the rewritten file (Open ‖ Snapshot)
    /// replays to exactly the applied state — the same property Task 9 pins for the
    /// journal itself.
    #[tokio::test]
    async fn batch_compacts_the_journal_once_it_passes_compact_at() {
        let dir = std::env::temp_dir().join(format!("ava1-recv-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let job = [0x44; 16];
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 2 * GROUP + 7,
                mtime: 0,
                path: "big".into(),
                root: None,
            }],
        });
        let open = JnlOpen {
            job_id: job,
            manifest_hash: m.hash(),
            kind: gen::JOB_DOWNLOAD,
            flags: 0,
            staged: 0,
            root: "/dest".into(),
        };
        let mut jnl = Journal::create(&dir, &open).unwrap();
        let mut st = State::default();
        st.apply(&Record::Open(open.clone()));
        // Grow the journal past COMPACT_AT with the durable-file batches a long upload
        // appends (a 400-run batch is ~5.6 KiB on disk, so ~190 of them cross 1 MiB).
        let mut i = 0u32;
        while jnl.len() <= journal::COMPACT_AT {
            let rec = Record::Batch(JnlBatch {
                files: (0..400)
                    .map(|k| gen::FileRun {
                        first: i * 400 + 1 + k,
                        count: 1,
                    })
                    .collect(),
                ranges: Vec::new(),
                roots: Vec::new(),
                pack_len: None,
                pack_offset: None,
                pack_segment: None,
            });
            st.apply(&rec);
            jnl.append(&rec).unwrap();
            i += 1;
            assert!(i < 10_000, "the journal never crossed COMPACT_AT");
        }
        let before = jnl.len();
        assert!(before > journal::COMPACT_AT);
        // One large file with a freshly written range: the batch syncs, journals the range
        // and — now past COMPACT_AT — compacts.
        let mut large = HashMap::new();
        large.insert(0u32, new_large(&dir, &m, 0).unwrap());
        large.get_mut(&0).unwrap().written.insert(0, GROUP);
        let (link, _keep_link) = test_link(job);
        let sink: Arc<dyn Sink> = Arc::new(NoopSink);
        let mut done = BTreeSet::new();
        let pg = Arc::default();
        let snap = snapshot_batch(
            job,
            link.control.clone(),
            &sink,
            &m,
            jnl,
            &open,
            st,
            &pg,
            &mut Vec::new(),
            &mut large,
        )
        .unwrap();
        let out = batch_task(snap).await.unwrap();
        fold_batch(&mut done, &mut large, &pg, &m, &out);
        let (jnl, st) = (out.jnl, out.st);
        assert!(
            jnl.len() <= journal::COMPACT_AT,
            "a compaction brings the journal back under the threshold: {}",
            jnl.len()
        );
        assert!(
            jnl.len() < before,
            "the journal shrank: {} -> {}",
            before,
            jnl.len()
        );
        drop(jnl);
        let (_, recs) = Journal::open(&dir).unwrap();
        let mut st2 = State::default();
        for r in &recs {
            st2.apply(r);
        }
        assert_eq!(st2, st, "a compaction loses no state");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_resumed_job_waits_longer_and_only_a_batch_with_work_is_progress() {
        assert_eq!(progress_limit(None, false), PROGRESS_DEADLINE);
        assert_eq!(progress_limit(None, true), RESUME_PROGRESS_DEADLINE);
        assert_eq!(
            progress_limit(Some(Duration::from_millis(400)), true),
            Duration::from_secs(10)
        );
        assert_eq!(
            progress_limit(Some(Duration::from_millis(400)), false),
            Duration::from_millis(400)
        );
        assert_eq!(PROGRESS_DEADLINE * RESUME_FACTOR, RESUME_PROGRESS_DEADLINE);
        // A long sync that returns nothing durable still counts; the empty periodic one does not.
        assert!(batch_is_progress(true));
        assert!(!batch_is_progress(false));
    }

    #[test]
    fn the_final_sync_does_not_wait_for_the_tick() {
        let soon = Duration::from_millis(5);
        // Mid-stream: not due before the interval.
        assert!(!sync_due(soon, false));
        // Every file is in: due at once, whatever the interval says.
        assert!(sync_due(soon, true));
        // And the regular cadence still fires.
        assert!(sync_due(SYNC_EVERY, false));
    }

    #[test]
    fn a_bundle_write_retries_a_bad_root_and_writes_the_rest() {
        use std::sync::Mutex as M;
        #[derive(Default)]
        struct Rec(M<Vec<u32>>);
        impl Sink for Rec {
            fn prepare(&self, _m: &Manifest) -> io::Result<()> {
                Ok(())
            }
            fn write_at(&self, _i: u32, _o: u64, _d: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn write_whole(&self, id: u32, _d: &[u8]) -> io::Result<()> {
                self.0.lock().unwrap().push(id);
                Ok(())
            }
            fn sync(&self, _ids: &[u32]) -> io::Result<()> {
                Ok(())
            }
            fn read_at(&self, _i: u32, _o: u64, _b: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
            fn commit(&self, _i: u32) -> io::Result<()> {
                Ok(())
            }
            fn finish(&self) -> io::Result<()> {
                Ok(())
            }
        }
        let sink = Rec::default();
        let good = b"abc".to_vec();
        let root = *blake3::hash(&good).as_bytes();
        let out = write_bundle(
            &sink,
            vec![
                (1, root, good.clone()),
                (2, [0u8; 32], good.clone()),
                (3, root, good),
            ],
            777,
        )
        .unwrap();
        assert_eq!(out.ok, vec![1, 3]);
        assert_eq!(out.retry, vec![(2, gen::RETRY_VERIFY)]);
        assert_eq!(out.credit, 777);
        assert_eq!(*sink.0.lock().unwrap(), vec![1, 3]); // the bad one was never written
    }

    // ---- commit off the sink lock, bounded parallel (review 009 #5) ----

    fn large_manifest(n: u32, size: u64) -> Manifest {
        Manifest {
            entries: (0..n)
                .map(|i| Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size,
                    mtime: 0,
                    path: format!("f{i}"),
                    root: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_commit_holds_no_sink_lock_while_it_fsyncs_and_renames() {
        let d = std::env::temp_dir().join(format!("p5a-commitlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let m = large_manifest(2, 3 * GROUP);
        let sink =
            Arc::new(LocalSink::new(d.join("new"), false).with_log(false, PackOpts::default()));
        sink.prepare(&m).unwrap();
        for id in 0..2 {
            sink.write_at(id, 0, &vec![id as u8 + 1; GROUP as usize])
                .unwrap();
        }
        // The commit of file 0 parks right after it has taken its descriptor, where the fsync and
        // the rename would run.
        let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let parked_tx = Mutex::new(parked_tx);
        *sink.commit_hook.lock().unwrap() = Some(Box::new(move |_id| {
            parked_tx.lock().unwrap().send(()).unwrap();
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(20));
        }));
        let s2 = sink.clone();
        let t = std::thread::spawn(move || s2.commit(0));
        parked_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        // Other sink calls proceed while that commit is parked: a write, a sync, a read.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let s3 = sink.clone();
        std::thread::spawn(move || {
            s3.write_at(1, GROUP, &vec![9; GROUP as usize]).unwrap();
            s3.sync(&[1]).unwrap();
            let mut b = [0u8; 4];
            s3.read_at(1, 0, &mut b).unwrap();
            done_tx.send(()).unwrap();
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a sink call waited behind a parked commit: the lock is held");
        go_tx.send(()).unwrap();
        t.join().unwrap().unwrap();
        assert!(d.join("new.ava-part/f0").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_commits_of_one_path_never_overlap() {
        let d = std::env::temp_dir().join(format!("p5a-commitrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let m = large_manifest(1, 2 * GROUP);
        let sink =
            Arc::new(LocalSink::new(d.join("new"), false).with_log(false, PackOpts::default()));
        sink.prepare(&m).unwrap();
        sink.write_at(0, 0, &vec![1; GROUP as usize]).unwrap();
        let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let parked_tx = Mutex::new(parked_tx);
        *sink.commit_hook.lock().unwrap() = Some(Box::new(move |_id| {
            parked_tx.lock().unwrap().send(()).unwrap();
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(20));
        }));
        let s2 = sink.clone();
        let t = std::thread::spawn(move || s2.commit(0));
        parked_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        // A second commit of the same file while the first is in flight is refused, not raced.
        let e = sink.commit(0).unwrap_err();
        assert!(e.to_string().contains("already"), "{e}");
        go_tx.send(()).unwrap();
        t.join().unwrap().unwrap();
        // Done: the path is free again (a re-run after a reset commits normally).
        *sink.commit_hook.lock().unwrap() = None;
        sink.write_at(0, 0, &vec![2; 2 * GROUP as usize]).unwrap();
        sink.commit(0).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_crash_between_the_commit_and_its_journal_record_resumes() {
        // The rename is in place but no record names the file done. Staged (a new folder): the
        // bytes sit in the staging folder, the resume check re-reads them and the commit is a
        // no-op. Into an existing folder: the part file is gone, so the resume check's read fails
        // (the file is reset and resent) and a second run commits over the first run's file.
        for staged in [true, false] {
            let d = std::env::temp_dir()
                .join(format!("p5a-commitcrash-{staged}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let root = d.join("new");
            if !staged {
                std::fs::create_dir_all(&root).unwrap();
            }
            let m = large_manifest(1, 2 * GROUP);
            let first = LocalSink::new(root.clone(), false).with_log(false, PackOpts::default());
            first.prepare(&m).unwrap();
            first.write_at(0, 0, &vec![1; 2 * GROUP as usize]).unwrap();
            first.sync(&[0]).unwrap();
            first.commit(0).unwrap();
            drop(first); // the crash: nothing journaled
            let second = LocalSink::new(root.clone(), false).with_log(false, PackOpts::default());
            second.prepare(&m).unwrap();
            let mut b = vec![0u8; GROUP as usize];
            let intact = second.read_at(0, 0, &mut b).is_ok();
            assert_eq!(
                intact, staged,
                "staged bytes verify; a lost part file is reset"
            );
            let fill = if intact { 1 } else { 7 };
            if !intact {
                second
                    .write_at(0, 0, &vec![fill; 2 * GROUP as usize])
                    .unwrap();
                second.sync(&[0]).unwrap();
            }
            second.commit(0).unwrap();
            second.sync_committed(&[0]).unwrap();
            second.finish().unwrap();
            let got = std::fs::read(root.join("f0")).unwrap();
            assert!(got.len() == 2 * GROUP as usize && got.iter().all(|&x| x == fill));
            assert!(!root.join("f0.ava-part").exists());
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// A sink whose commit sleeps, counting how many run at once and what the journal held when
    /// the committed names were synced.
    struct SlowCommit {
        delay: Duration,
        now: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
        finished: std::sync::atomic::AtomicUsize,
        jd: PathBuf,
        at_sync: Mutex<Option<(usize, usize)>>,
        fail: Option<u32>,
    }

    impl Sink for SlowCommit {
        fn prepare(&self, _: &Manifest) -> io::Result<()> {
            Ok(())
        }
        fn write_at(&self, _: u32, _: u64, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn write_whole(&self, _: u32, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn sync(&self, _: &[u32]) -> io::Result<()> {
            Ok(())
        }
        fn read_at(&self, _: u32, _: u64, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.now.fetch_sub(1, Ordering::SeqCst);
            if self.fail == Some(id) {
                return Err(io::Error::other("rename failed"));
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn sync_committed(&self, ids: &[u32]) -> io::Result<()> {
            // every commit has returned, and the journal does not yet call any of them done
            let (j, recs) = Journal::open(&self.jd).unwrap();
            drop(j);
            let mut st = State::default();
            for r in &recs {
                st.apply(r);
            }
            let named = ids.iter().filter(|i| st.done.contains(i)).count();
            *self.at_sync.lock().unwrap() = Some((self.finished.load(Ordering::SeqCst), named));
            Ok(())
        }
        fn finish(&self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn run_commit_batch(
        tag: &str,
        n: u32,
        sink: Arc<SlowCommit>,
        dir: &Path,
    ) -> (Result<BatchDone, SendError>, Duration) {
        let job = [0x55; 16];
        let m = Arc::new(large_manifest(n, 16));
        let open = JnlOpen {
            job_id: job,
            manifest_hash: m.hash(),
            kind: gen::JOB_DOWNLOAD,
            flags: 0,
            staged: 0,
            root: format!("/{tag}"),
        };
        let jnl = Journal::create(dir, &open).unwrap();
        let mut st = State::default();
        st.apply(&Record::Open(open.clone()));
        let zero_root = *blake3::hash(&[0u8; 16]).as_bytes();
        let mut large = HashMap::new();
        for id in 0..n {
            let mut l = new_large(dir, &m, id).unwrap();
            l.written.insert(0, 16);
            l.root = Some(zero_root);
            large.insert(id, l);
        }
        let (link, _keep) = test_link(job);
        let s: Arc<dyn Sink> = sink;
        let pg = Arc::default();
        let snap = snapshot_batch(
            job,
            link.control.clone(),
            &s,
            &m,
            jnl,
            &open,
            st,
            &pg,
            &mut Vec::new(),
            &mut large,
        )
        .unwrap();
        let t = Instant::now();
        let r = batch_task(snap).await;
        (r, t.elapsed())
    }

    fn slow(dir: &Path, fail: Option<u32>) -> Arc<SlowCommit> {
        Arc::new(SlowCommit {
            delay: Duration::from_millis(200),
            now: Default::default(),
            peak: Default::default(),
            finished: Default::default(),
            jd: dir.to_path_buf(),
            at_sync: Mutex::new(None),
            fail,
        })
    }

    #[tokio::test]
    async fn eight_large_commits_overlap_and_the_names_sync_before_the_record() {
        let dir = std::env::temp_dir().join(format!("p5a-commitpar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink = slow(&dir, None);
        let (r, took) = run_commit_batch("par", 8, sink.clone(), &dir).await;
        let out = r.unwrap();
        assert_eq!(out.committed.len(), 8);
        // 8 files x 200 ms: serial is 1.6 s, COMMIT_PAR = 4 is ~0.4 s. A generous bound.
        assert!(
            took < Duration::from_millis(1100),
            "commits ran serially: {took:?}"
        );
        let peak = sink.peak.load(Ordering::SeqCst);
        assert!((2..=COMMIT_PAR).contains(&peak), "peak in flight {peak}");
        // names synced after every commit returned and before the journal named any file done
        assert_eq!(*sink.at_sync.lock().unwrap(), Some((8, 0)));
        // and the journal then names all eight
        assert_eq!(out.st.done.len(), 8);
        let (_, recs) = Journal::open(&dir).unwrap();
        let mut st = State::default();
        for r in &recs {
            st.apply(r);
        }
        assert_eq!(st.done.len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_commit_ends_the_batch_after_the_others_have_returned_and_journals_the_rest() {
        let dir = std::env::temp_dir().join(format!("p5a-commitfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink = slow(&dir, Some(3));
        let (r, _) = run_commit_batch("fail", 8, sink.clone(), &dir).await;
        assert!(r.is_err());
        // no commit is still running when the error comes back, and no file was called done
        assert_eq!(sink.now.load(Ordering::SeqCst), 0);
        assert_eq!(sink.finished.load(Ordering::SeqCst), 7);
        // the seven that went through are synced and journaled; the failed one is not
        assert_eq!(*sink.at_sync.lock().unwrap(), Some((7, 0)));
        let (_, recs) = Journal::open(&dir).unwrap();
        let mut st = State::default();
        for r in &recs {
            st.apply(r);
        }
        assert_eq!(st.done.len(), 7);
        assert!(!st.done.contains(&3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
