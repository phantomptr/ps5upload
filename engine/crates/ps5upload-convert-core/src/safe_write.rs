//! Writing on the PS5 without hurting it: PS5 Dump Forge's measured rules (U1-U7 in
//! `payload/convert`'s design notes), for the converter that runs on the console.
//!
//! - [`probe`] the destination once before writing (U1): its filesystem, free space, whether it
//!   is read-only, the largest file it holds (FAT32: 4 GiB).
//! - [`Bounded`] makes the written bytes durable every so often (U5): sustained multi-GB writes
//!   panicked the console's kernel from dirty-page build-up. It also never leaves a hole: a
//!   forward seek past the end writes zeros (a sparse file collapses to 2-3 MiB/s on PS5 UFS).
//! - [`sync`] retries a failed fsync, and a sync that only succeeded on a retry fails (U4).
//! - [`remove_file`] makes a removal durable by syncing the folder (U6: Sony's exFAT shows the
//!   freed space as used until then).
//! - [`rename`] refuses to rename across filesystems: on the PS5 that panics the kernel.
//!
//! The build code takes these only in the PS5 build (`cfg(target_env = "ps5")`): a computer
//! writes as it always has. They are plain std code, so the tests run them on any computer.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

/// Whether this build is the console's.
pub const ON_CONSOLE: bool = cfg!(target_env = "ps5");

/// Dirty bytes [`Bounded`] lets build up before it syncs, at first.
pub const SYNC_EVERY: u64 = 32 << 20;
/// The range [`Bounded`] adapts its interval in: a quick sync doubles it, a slow one halves it.
pub const SYNC_MIN: u64 = 16 << 20;
pub const SYNC_MAX: u64 = 256 << 20;
/// A sync slower than this halves the interval; one faster than [`SYNC_QUICK`] doubles it.
const SYNC_SLOW: Duration = Duration::from_millis(1500);
const SYNC_QUICK: Duration = Duration::from_millis(150);
/// The waits before each retry of a failed fsync.
const SYNC_RETRIES: [Duration; 2] = [Duration::from_millis(50), Duration::from_millis(250)];

/// Something whose written bytes can be made durable: a file, or a test's stand-in.
pub trait Durable {
    fn sync_data(&mut self) -> io::Result<()>;
}

impl Durable for std::fs::File {
    fn sync_data(&mut self) -> io::Result<()> {
        std::fs::File::sync_data(self)
    }
}

impl Durable for &mut std::fs::File {
    fn sync_data(&mut self) -> io::Result<()> {
        std::fs::File::sync_data(self)
    }
}

/// An fsync error that means "this filesystem has no fsync", not a failure.
fn no_sync(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    )
}

/// Worth trying again: interrupted, or the device busy for a moment.
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Makes `out`'s bytes durable. A transient failure is tried again after a pause; if a retry is
/// what succeeded, the bytes may not be what was written (the drive dropped a write it had
/// acknowledged), so that fails too. A filesystem without fsync passes.
pub fn sync(out: &mut dyn Durable) -> io::Result<()> {
    let first = match out.sync_data() {
        Ok(()) => return Ok(()),
        Err(e) if no_sync(&e) => return Ok(()),
        Err(e) => e,
    };
    if !transient(&first) {
        return Err(first);
    }
    let mut last = first;
    for (n, wait) in SYNC_RETRIES.iter().enumerate() {
        std::thread::sleep(*wait);
        match out.sync_data() {
            Ok(()) => {
                return Err(io::Error::other(format!(
                    "the drive saved the data only on try {} ({last}); what it wrote is not \
                     trusted",
                    n + 2
                )))
            }
            Err(e) if transient(&e) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// A writer that syncs every so many dirty bytes (adapting to how long a sync takes) and fills
/// a forward seek past the end with zeros, so the file it writes has no holes. Seeks within what
/// is written already move as asked.
pub struct Bounded<W: Write + Seek + Durable> {
    inner: W,
    pos: u64,
    end: u64,
    dirty: u64,
    every: u64,
    /// Syncs made, for the log and the tests.
    pub syncs: u64,
}

impl<W: Write + Seek + Durable> Bounded<W> {
    /// `inner` positioned at its start, empty.
    pub fn new(inner: W) -> Self {
        Self::at(inner, 0)
    }

    /// `inner` holding `end` bytes already and positioned there.
    pub fn at(inner: W, end: u64) -> Self {
        Self {
            inner,
            pos: end,
            end,
            dirty: 0,
            every: SYNC_EVERY,
            syncs: 0,
        }
    }

    /// The bytes before the next sync, now.
    pub fn interval(&self) -> u64 {
        self.every
    }

    fn sync_now(&mut self) -> io::Result<()> {
        let started = std::time::Instant::now();
        sync(&mut self.inner)?;
        let took = started.elapsed();
        self.syncs += 1;
        self.dirty = 0;
        if took >= SYNC_SLOW {
            self.every = (self.every / 2).max(SYNC_MIN);
        } else if took <= SYNC_QUICK {
            self.every = (self.every * 2).min(SYNC_MAX);
        }
        Ok(())
    }

    /// Writes zeros from the end up to `to`.
    fn fill_to(&mut self, to: u64) -> io::Result<()> {
        if self.pos != self.end {
            self.inner.seek(SeekFrom::Start(self.end))?;
            self.pos = self.end;
        }
        let zeros = [0u8; 64 << 10];
        while self.pos < to {
            let n = (to - self.pos).min(zeros.len() as u64) as usize;
            self.write_all(&zeros[..n])?;
        }
        Ok(())
    }

    /// The writer back, its bytes synced.
    pub fn finish(mut self) -> io::Result<W> {
        self.inner.flush()?;
        self.sync_now()?;
        Ok(self.inner)
    }
}

impl<W: Write + Seek + Durable> Write for Bounded<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.pos += n as u64;
        self.end = self.end.max(self.pos);
        self.dirty += n as u64;
        if self.dirty >= self.every {
            self.inner.flush()?;
            self.sync_now()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write + Seek + Durable> Seek for Bounded<W> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.end.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek out of range"))?;
        if target > self.end {
            self.fill_to(target)?;
        } else if target != self.pos {
            self.inner.seek(SeekFrom::Start(target))?;
            self.pos = target;
        }
        Ok(self.pos)
    }
}

/// A handle [`GrowthSync`] syncs `path` through: with write access (Windows refuses
/// `FlushFileBuffers` on a read-only handle, PermissionDenied), never creating or truncating the
/// file the builder writes. `None` while it does not exist yet.
fn open_for_sync(path: &std::path::Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open(path).ok()
}

/// U5 for a file written by code that keeps its own `File` (the `.pkg` builder): a thread that
/// looks at the file every `poll` and, each time it has grown by `every` bytes since the last
/// sync, makes it durable through a handle of its own (a sync covers the file, whichever handle
/// asks). A failed sync raises `failed` (the job's cancel flag: the build stops) and is what
/// [`GrowthSync::finish`] returns.
pub struct GrowthSync {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<io::Result<u64>>>,
}

impl GrowthSync {
    /// Starts watching `path` (which may not exist yet).
    pub fn start(
        path: std::path::PathBuf,
        every: u64,
        poll: Duration,
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = std::sync::Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("ps5u-convert".into())
            .spawn(move || {
                let mut file: Option<std::fs::File> = None;
                let (mut synced, mut syncs) = (0u64, 0u64);
                let mut look = |file: &mut Option<std::fs::File>, last: bool| -> io::Result<()> {
                    if file.is_none() {
                        *file = open_for_sync(&path);
                    }
                    let Some(f) = file.as_mut() else {
                        return Ok(());
                    };
                    let len = f.metadata()?.len();
                    if len >= synced.saturating_add(every) || (last && len != synced) {
                        sync(f)?;
                        synced = len;
                        syncs += 1;
                    }
                    Ok(())
                };
                loop {
                    let last = stopped.load(Ordering::Relaxed);
                    if let Err(e) = look(&mut file, last) {
                        failed.store(true, Ordering::Relaxed);
                        return Err(e);
                    }
                    if last {
                        return Ok(syncs);
                    }
                    std::thread::sleep(poll);
                }
            })
            .expect("a thread to sync the output");
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops watching after one last sync of what was written since the last one: the syncs
    /// made, or the failure of one.
    pub fn finish(mut self) -> io::Result<u64> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        match self.thread.take().map(|t| t.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(io::Error::other("the sync thread panicked")),
            None => Ok(0),
        }
    }
}

impl Drop for GrowthSync {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Removes the file at `path` and syncs its folder, so the freed space shows as free.
pub fn remove_file(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")));
    Ok(())
}

/// How the build code removes what it wrote: [`remove_file`] on the console, a plain removal
/// elsewhere.
pub fn discard(path: &Path) -> io::Result<()> {
    if ON_CONSOLE {
        remove_file(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Syncs a folder's entries, best effort (a filesystem may not allow it).
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Why `from` must not be renamed into the folder `to_dir`, or `Ok`: on another filesystem a
/// rename is a copy the kernel makes (or, on the PS5, a kernel panic). The device numbers are
/// compared; off Unix there is nothing to compare and the rename itself refuses. `from` is
/// what is renamed (a link itself, if it is one); `to_dir` is followed, since the new name lands
/// where a linked folder points.
pub fn same_device(from: &Path, to_dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = |m: std::io::Result<std::fs::Metadata>, p: &Path| {
            m.map(|m| m.dev())
                .map_err(|e| format!("{}: {e}", p.display()))
        };
        let a = dev(std::fs::symlink_metadata(from), from)?;
        let b = dev(std::fs::metadata(to_dir), to_dir)?;
        if a != b {
            return Err(format!(
                "{} and {} are on different drives: nothing is renamed across drives",
                from.display(),
                to_dir.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (from, to_dir);
    Ok(())
}

/// `std::fs::rename`, and on the console only after [`same_device`] said yes.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if ON_CONSOLE {
        let dir = match to.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        same_device(from, dir).map_err(io::Error::other)?;
    }
    ps5upload_fpkg::finish::rename(from, to)
}

/// [`rename`] that never replaces what is at `to`: a finished output only ever gets a free
/// name. Where the system has a rename that refuses an existing target (Linux `renameat2` with
/// `RENAME_NOREPLACE`, macOS `renamex_np` with `RENAME_EXCL`) it is used; elsewhere (FreeBSD and
/// the PS5, Windows, a filesystem that has no such rename) `to` is checked just before a plain
/// rename, which leaves the moment between the two for something to appear there. A taken name
/// is `AlreadyExists`. Never across drives: on the console [`same_device`] says yes first, and a
/// computer's rename refuses another filesystem itself.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    if ON_CONSOLE {
        let dir = match to.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        same_device(from, dir).map_err(io::Error::other)?;
    }
    // Tried again while a virus scanner or indexer briefly holds the file (Windows).
    ps5upload_fpkg::finish::retry_held(|| match rename_excl(from, to) {
        Some(r) => r,
        None => {
            if to.symlink_metadata().is_ok() {
                return Err(taken(to));
            }
            std::fs::rename(from, to)
        }
    })
}

/// [`rename_new`] for a build's last step, `partial` being a finished and checked output. On a
/// failure `kept` is set and the error says where the output is: the caller leaves `partial`
/// alone instead of removing it as it does a failed build's. A name something else took while
/// the build ran is not such a failure: that stays the refusal it was, the partial removed.
pub fn name_finished(partial: &Path, target: &Path, kept: &mut bool) -> Result<(), String> {
    rename_new(partial, target).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            return e.to_string();
        }
        *kept = true;
        ps5upload_fpkg::finish::kept_message(partial, target, &e)
    })
}

fn taken(to: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} appeared while it was written", to.display()),
    )
}

/// The system's own no-replace rename, or `None` where there is none (or the filesystem
/// doesn't take it) and the caller checks and renames.
#[cfg(any(
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
    target_os = "macos"
))]
fn rename_excl(from: &Path, to: &Path) -> Option<io::Result<()>> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).ok();
    let (a, b) = (c(from)?, c(to)?);
    // SAFETY: two NUL-terminated paths that outlive the call.
    #[cfg(target_os = "linux")]
    // The system call itself, not libc's wrapper: musl (the Docker engine's Alpine build) has no
    // `renameat2` function, and an older glibc may not either. A kernel without it says ENOSYS.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as libc::c_int
    };
    // SAFETY: as above.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        return Some(Ok(()));
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EEXIST) => Some(Err(taken(to))),
        // No such rename here, or this filesystem doesn't take it: check and rename.
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP) => None,
        _ => Some(Err(e)),
    }
}

#[cfg(not(any(
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
    target_os = "macos"
)))]
fn rename_excl(_from: &Path, _to: &Path) -> Option<io::Result<()>> {
    None
}

/// What a destination folder is, measured once before a job writes there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The filesystem's name as the kernel gives it (`ufs`, `exfatfs`, `msdosfs`, ...), or empty
    /// where it can't be read.
    pub fs_type: String,
    /// Bytes free to this process.
    pub free: Option<u64>,
    pub read_only: bool,
    /// The largest file the filesystem holds, when it has a limit worth knowing (FAT: 4 GiB - 1).
    pub max_file: Option<u64>,
}

impl Probe {
    /// Why `bytes` of output (`one_file`: as one file, an image) can't go here, or `None`.
    /// `spare` is the room kept beyond the output.
    pub fn refusal(&self, bytes: u64, one_file: bool, spare: u64) -> Option<String> {
        if self.read_only {
            return Some("the drive is read-only".into());
        }
        if one_file {
            if let Some(why) = self.file_refusal(bytes, "image") {
                return Some(why);
            }
        }
        match self.free {
            Some(free) if free < bytes.saturating_add(spare) => Some(format!(
                "only {:.1} GiB free, and this needs about {:.1} GiB",
                free as f64 / (1u64 << 30) as f64,
                bytes.saturating_add(spare) as f64 / (1u64 << 30) as f64
            )),
            _ => None,
        }
    }

    /// Why one file of `bytes` (a `what`: an image, a pack volume) can't go here, for a drive
    /// with a file size limit (FAT32: 4 GiB), or `None`.
    pub fn file_refusal(&self, bytes: u64, what: &str) -> Option<String> {
        let max = self.max_file.filter(|m| bytes > *m)?;
        Some(format!(
            "the drive ({}) holds files of at most {:.1} GiB, and this {what} is at least {:.1} GiB",
            self.fs_type,
            max as f64 / (1u64 << 30) as f64,
            bytes as f64 / (1u64 << 30) as f64
        ))
    }
}

/// On the console, why one file of `bytes` (a `what`) can't be written at `output` (its folder
/// is measured: FAT32 holds 4 GiB at most), or `None`: for a size known only once a build has
/// planned or packed, checked before the first byte is written. A computer finds out as it
/// writes, as it always has.
pub fn too_big_for(output: &Path, bytes: u64, what: &str) -> Option<String> {
    if !ON_CONSOLE {
        return None;
    }
    let dir = output.parent().filter(|p| !p.as_os_str().is_empty())?;
    probe(dir).ok()?.file_refusal(bytes, what)
}

/// The largest file a filesystem of this name holds, where that is less than a game.
fn max_file_of(fs_type: &str) -> Option<u64> {
    match fs_type {
        "msdosfs" | "msdos" | "vfat" | "fat" | "fat32" => Some((4u64 << 30) - 1),
        _ => None,
    }
}

/// Measures the folder `dir` (U1).
pub fn probe(dir: &Path) -> io::Result<Probe> {
    let meta = std::fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    let mut p = Probe {
        fs_type: String::new(),
        free: None,
        read_only: meta.permissions().readonly(),
        max_file: None,
    };
    statfs_into(dir, &mut p)?;
    p.max_file = max_file_of(&p.fs_type);
    Ok(p)
}

/// A statfs count as bytes or blocks: a negative one (FreeBSD's signed `f_bavail` inside the
/// root reserve) is none, never a huge number.
#[cfg_attr(not(unix), allow(dead_code))]
fn unsigned<T: TryInto<u64>>(n: T) -> u64 {
    n.try_into().unwrap_or(0)
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn statfs_into(dir: &Path, p: &mut Probe) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("a NUL in the path"))?;
    // SAFETY: `st` is plain data the call fills; `c` is a NUL-terminated path.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let name: Vec<u8> = st
        .f_fstypename
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as u8)
        .collect();
    p.fs_type = String::from_utf8_lossy(&name).into_owned();
    // `f_bavail` is signed on FreeBSD (the PS5): negative inside the root reserve, not huge.
    p.free = Some(unsigned(st.f_bavail).saturating_mul(unsigned(st.f_bsize)));
    p.read_only |= (st.f_flags as u64) & (libc::MNT_RDONLY as u64) != 0;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "freebsd", target_os = "macos"))))]
fn statfs_into(dir: &Path, p: &mut Probe) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("a NUL in the path"))?;
    // SAFETY: `st` is plain data the call fills; `c` is a NUL-terminated path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    p.free = Some(unsigned(st.f_bavail).saturating_mul(unsigned(st.f_frsize)));
    // `f_flag` and `ST_RDONLY` are `c_ulong` on some systems and `u64` on others.
    #[allow(clippy::unnecessary_cast)]
    let flags = st.f_flag as u64 & libc::ST_RDONLY as u64;
    p.read_only |= flags != 0;
    Ok(())
}

#[cfg(not(unix))]
fn statfs_into(_dir: &Path, _p: &mut Probe) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The sync's own handle can write (Windows syncs only such a handle), and opening it neither
    /// creates the file nor cuts what the builder wrote.
    #[test]
    fn the_sync_handle_writes_and_never_creates_or_truncates() {
        let dir = std::env::temp_dir().join(format!("ps5u-sync-handle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.pkg.partial");
        assert!(open_for_sync(&path).is_none());
        assert!(!path.exists());
        std::fs::write(&path, b"written so far").unwrap();
        let mut f = open_for_sync(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"written so far");
        // Needs write access on every platform (a read-only handle is refused).
        f.set_len(14).unwrap();
        sync(&mut f).unwrap();
        drop(f);
        assert_eq!(std::fs::read(&path).unwrap(), b"written so far");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file another writer grows is synced each time it grew by the interval, once more at
    /// the end, and a file that never appears costs nothing.
    #[test]
    fn a_growing_file_is_synced_as_it_grows() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("ps5u-growth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.pkg.partial");
        let failed = Arc::new(AtomicBool::new(false));
        let watch = GrowthSync::start(
            path.clone(),
            1 << 20,
            Duration::from_millis(2),
            Arc::clone(&failed),
        );
        let mut f = std::fs::File::create(&path).unwrap();
        for _ in 0..4 {
            f.write_all(&vec![7u8; (1 << 20) + 1]).unwrap();
            std::thread::sleep(Duration::from_millis(40));
        }
        f.write_all(b"tail").unwrap();
        let syncs = watch.finish().unwrap();
        // Four growth steps and the end: up to 5. A busy machine (CI) can see two steps in one
        // look and sync once for both, so at least one mid-growth sync plus the final one.
        assert!((2..=5).contains(&syncs), "{syncs}");
        assert!(!failed.load(Ordering::Relaxed));
        let none = GrowthSync::start(
            dir.join("never"),
            1,
            Duration::from_millis(1),
            Arc::clone(&failed),
        );
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(none.finish().unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file in memory that counts syncs and fails them as told.
    struct Mem {
        data: Cursor<Vec<u8>>,
        syncs: u32,
        fail: Vec<io::ErrorKind>,
    }

    impl Mem {
        fn new() -> Self {
            Self {
                data: Cursor::new(Vec::new()),
                syncs: 0,
                fail: Vec::new(),
            }
        }
    }

    impl Write for Mem {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.data.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for Mem {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.data.seek(to)
        }
    }

    impl Durable for Mem {
        fn sync_data(&mut self) -> io::Result<()> {
            self.syncs += 1;
            match self.fail.pop() {
                Some(kind) => Err(io::Error::new(kind, "test")),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn it_syncs_every_interval_and_fills_holes_with_zeros() {
        let mut w = Bounded::new(Mem::new());
        let chunk = vec![1u8; 1 << 20];
        for _ in 0..(SYNC_EVERY >> 20) - 1 {
            w.write_all(&chunk).unwrap();
        }
        assert_eq!(w.syncs, 0);
        w.write_all(&chunk).unwrap();
        assert_eq!(w.syncs, 1);
        // A quick sync widens the interval, up to its cap.
        assert!(w.interval() > SYNC_EVERY && w.interval() <= SYNC_MAX);
        let end = SYNC_EVERY;
        // Forward past the end: zeros, not a hole.
        w.seek(SeekFrom::Start(end + 100)).unwrap();
        w.write_all(b"xy").unwrap();
        // Back inside: overwrites in place.
        w.seek(SeekFrom::Start(10)).unwrap();
        w.write_all(b"z").unwrap();
        assert_eq!(w.seek(SeekFrom::End(0)).unwrap(), end + 102);
        let mem = w.finish().unwrap();
        let data = mem.data.into_inner();
        assert_eq!(data.len() as u64, end + 102);
        assert_eq!(data[10], b'z');
        assert!(data[end as usize..end as usize + 100]
            .iter()
            .all(|&b| b == 0));
        assert_eq!(&data[end as usize + 100..], b"xy");
    }

    #[test]
    fn a_sync_that_needs_a_retry_fails_and_one_without_fsync_passes() {
        let mut m = Mem::new();
        assert!(sync(&mut m).is_ok());
        // Interrupted once, then fine: the data is not trusted.
        m.fail = vec![io::ErrorKind::Interrupted];
        let e = sync(&mut m).unwrap_err();
        assert!(e.to_string().contains("only on try 2"), "{e}");
        // A hard error is the error.
        m.fail = vec![io::ErrorKind::PermissionDenied];
        assert_eq!(
            sync(&mut m).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // Interrupted every time: the last error.
        m.fail = vec![io::ErrorKind::Interrupted; 3];
        assert_eq!(sync(&mut m).unwrap_err().kind(), io::ErrorKind::Interrupted);
        // No fsync on this filesystem.
        m.fail = vec![io::ErrorKind::Unsupported];
        assert!(sync(&mut m).is_ok());
    }

    #[test]
    fn a_full_write_failure_is_the_writers_error() {
        let mut w = Bounded::new(Mem::new());
        w.write_all(&vec![0u8; SYNC_EVERY as usize - 1]).unwrap();
        w.inner.fail = vec![io::ErrorKind::StorageFull];
        assert_eq!(
            w.write_all(b"ab").unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
    }

    #[test]
    fn the_probe_reads_this_drive_and_refuses_what_does_not_fit() {
        let dir = std::env::temp_dir();
        let p = probe(&dir).unwrap();
        assert!(!p.read_only);
        #[cfg(any(target_os = "freebsd", target_os = "macos"))]
        assert!(!p.fs_type.is_empty());
        #[cfg(unix)]
        assert!(p.free.is_some_and(|f| f > 0));
        assert!(probe(&dir.join("no-such-folder-here")).is_err());

        let fat = Probe {
            fs_type: "msdosfs".into(),
            free: Some(100 << 30),
            read_only: false,
            max_file: max_file_of("msdosfs"),
        };
        assert!(fat
            .refusal(5 << 30, true, 0)
            .unwrap()
            .contains("at most 4.0 GiB"));
        // A folder of 5 GiB is many files, each under the limit.
        assert_eq!(fat.refusal(5 << 30, false, 0), None);
        // One file checked on its own (a pack volume, an image's planned size).
        assert!(fat
            .file_refusal(5 << 30, "pack volume")
            .unwrap()
            .contains("at most 4.0 GiB, and this pack volume"));
        assert_eq!(fat.file_refusal((4 << 30) - 1, "image"), None);
        assert_eq!(
            Probe {
                max_file: None,
                ..fat.clone()
            }
            .file_refusal(u64::MAX, "image"),
            None
        );
        // Off the console nothing is measured for it.
        assert_eq!(too_big_for(&dir.join("x.exfat"), u64::MAX, "image"), None);
        let full = Probe {
            free: Some(1 << 30),
            ..fat.clone()
        };
        assert!(full.refusal(2 << 30, false, 0).unwrap().contains("free"));
        let ro = Probe {
            read_only: true,
            ..fat
        };
        assert_eq!(
            ro.refusal(1, false, 0).as_deref(),
            Some("the drive is read-only")
        );
    }

    #[test]
    fn a_negative_free_count_is_none_not_huge() {
        assert_eq!(unsigned(-3i64), 0);
        assert_eq!(unsigned(i64::MIN), 0);
        assert_eq!(unsigned(7i64), 7);
        assert_eq!(unsigned(u64::MAX), u64::MAX);
        assert_eq!(unsigned(4096u32), 4096);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_renamed_across_drives() {
        let tmp = std::env::temp_dir();
        assert!(same_device(&tmp, &tmp).is_ok());
        // `/dev` is its own filesystem on Linux, macOS and FreeBSD.
        let dev = Path::new("/dev");
        if std::fs::metadata(dev).is_ok() {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(dev).unwrap().dev() != std::fs::metadata(&tmp).unwrap().dev() {
                let e = same_device(&tmp, dev).unwrap_err();
                assert!(e.contains("different drives"), "{e}");
            }
        }
        assert!(same_device(&tmp.join("missing-file"), &tmp).is_err());
    }

    /// A destination folder that is a link to another drive is that drive, not the link's.
    #[cfg(unix)]
    #[test]
    fn a_linked_destination_folder_is_where_it_points() {
        use std::os::unix::fs::MetadataExt;
        let dev = Path::new("/dev");
        let tmp = std::env::temp_dir();
        let Ok(d) = std::fs::metadata(dev) else {
            return;
        };
        if d.dev() == std::fs::metadata(&tmp).unwrap().dev() {
            return;
        }
        let link = tmp.join(format!("ps5upload-safe-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(dev, &link).unwrap();
        let e = same_device(&tmp, &link);
        let _ = std::fs::remove_file(&link);
        assert!(e.unwrap_err().contains("different drives"));
    }

    #[test]
    fn a_new_name_never_replaces_what_is_there() {
        let dir = std::env::temp_dir().join(format!("ps5upload-safe-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("a.partial"), dir.join("a"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"theirs").unwrap();
        let e = rename_new(&from, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read(&to).unwrap(), b"theirs");
        assert!(from.is_file());
        // An empty folder there is not replaced either (a plain rename would, on Unix).
        std::fs::remove_file(&to).unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::create_dir(dir.join("d.partial")).unwrap();
        assert!(rename_new(&dir.join("d.partial"), &dir.join("d")).is_err());
        assert!(dir.join("d.partial").is_dir());
        // A free name: renamed.
        rename_new(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_removal_leaves_nothing() {
        let f = std::env::temp_dir().join(format!("ps5upload-safe-{}", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        remove_file(&f).unwrap();
        assert!(!f.exists());
        assert!(remove_file(&f).is_err());
    }
}
