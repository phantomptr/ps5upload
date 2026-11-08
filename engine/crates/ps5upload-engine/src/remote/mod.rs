//! Files on the user's own servers (SMB, FTP, FTPS, SFTP, WebDAV), read through one interface.
//!
//! A pick from a server is a `remote://<connection-id>/<path>` string (see [`path`]). The engine
//! resolves it against the saved connections and reads it through [`RemoteFs`]: listing for the
//! in-app browser, positioned reads for streaming installs and uploads, and whole-file copies for
//! the jobs that need the bytes on this computer.

pub mod api;
#[cfg(test)]
pub(crate) mod contract;
pub mod fetch;
pub mod ftp_fs;
pub mod hints;
pub mod host_key;
mod http_digest;
pub mod nfs_fs;
pub mod path;
pub mod pool;
pub mod range;
pub mod sftp_fs;
pub mod smb_fs;
pub mod source_fs;
pub mod store;
pub mod webdav_fs;

#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use serde::Serialize;

/// Most entries one listing call returns; the rest follow through `next_cursor`.
pub const PAGE: usize = 200;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")]
    BadPath(String),
    #[error("The connection '{0}' no longer exists")]
    UnknownConnection(String),
    #[error("Sign-in failed: {0}")]
    Auth(String),
    #[error("Can't reach {0}")]
    Unreachable(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
    /// A write would replace something already there (a rename's target, say).
    #[error("{0}")]
    Exists(String),
    /// The server (or this protocol) cannot do what was asked.
    #[error("{0}")]
    Unsupported(String),
    /// The server's identity (SFTP host key, FTPS certificate) is not the one the user accepted.
    #[error("Sign-in failed: {}", if *changed { "the server's key has changed since you last connected" } else { "unknown server key — accept it to connect" })]
    HostKey { fingerprint: String, changed: bool },
}

#[allow(clippy::double_must_use)] // async_trait expands to a must_use future
#[async_trait::async_trait]
pub trait RemoteFile: Send + Sync {
    fn size(&self) -> u64;
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError>;
}

#[allow(clippy::double_must_use)] // async_trait expands to a must_use future
#[async_trait::async_trait]
pub trait RemoteFs: Send + Sync {
    /// SFTP/FTP: the folder the server signed in to, as a server path (the start folder a
    /// connection that browses the whole server opens at when none is given).
    fn login_dir(&self) -> Option<String> {
        None
    }
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError>;
    async fn stat(&self, path: &str) -> Result<Entry, RemoteError>;
    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError>;
    /// Every regular file under `path`, as (path relative to `path`, entry), capped at `limit`.
    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError> {
        let never = std::sync::atomic::AtomicBool::new(false);
        self.walk_until(path, limit, &never).await
    }

    /// [`Self::walk`], ending with [`walk_cancelled`] between folders once `cancel` is set: one
    /// listing per folder takes minutes on a big tree.
    async fn walk_until(
        &self,
        path: &str,
        limit: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<(String, Entry)>, RemoteError>;

    // ── Writes: only what the Collection's organizer needs, all of it non-destructive ──

    /// Creates one folder (its parent must exist). A folder already there is success; a
    /// file by that name is [`RemoteError::Exists`].
    async fn create_dir(&self, path: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot create {path}"
        )))
    }

    /// Renames a file or folder within the server. Never replaces anything: when `to` is
    /// already there it fails with [`RemoteError::Exists`] and both stay as they were.
    async fn rename(&self, from: &str, _to: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot rename {from}"
        )))
    }

    /// Removes an empty folder. A folder with anything in it is refused and left as it is.
    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot remove {path}"
        )))
    }

    // ── Writing files: downloads from a console to a server (8.0.0), Convert's output ──

    /// Opens `path` to be written front to back, `size` bytes in all. `from` 0 creates the
    /// file, or empties one already there; a later `from` keeps the first `from` bytes of the
    /// file already there and writes on from that byte (only where [`Self::writes_from`] says
    /// it can). Every write of a file goes through here, [`Self::put_file`] too.
    async fn create_writer<'a>(
        &'a self,
        path: &str,
        _size: u64,
        _from: u64,
    ) -> Result<Box<dyn RemoteWriter + 'a>, RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot write {path}"
        )))
    }

    /// Whether [`Self::create_writer`] can go on from a byte past the start of a file already
    /// there (a write that was cut off). FTP and WebDAV write a file whole.
    fn writes_from(&self) -> bool {
        false
    }

    /// The bytes this user can still write under the folder `path`, as the server itself
    /// reports them; `None` when the protocol has no way to say (FTP, WebDAV). Never a guess.
    async fn free_space(&self, _path: &str) -> Result<Option<u64>, RemoteError> {
        Ok(None)
    }

    /// Writes the local file `src` to `path`, creating or truncating it, through
    /// [`Self::create_writer`]. `progress` hears the bytes as they go; a set `cancel` stops the
    /// write (the partial file is left for the caller to remove).
    async fn put_file(
        &self,
        path: &str,
        src: &std::path::Path,
        progress: &PutProgress,
    ) -> Result<(), RemoteError> {
        let file = tokio::fs::File::open(src).await.map_err(local_io)?;
        let size = file.metadata().await.map_err(local_io)?.len();
        let writer = self.create_writer(path, size, 0).await?;
        send_file(file, writer, progress, WRITE_STALL, path).await
    }

    /// Removes one file.
    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot remove {path}"
        )))
    }
}

/// What a walk ends with when told to stop. A job's error reads the same as a cancelled copy's.
pub(crate) fn walk_cancelled() -> RemoteError {
    RemoteError::Io("cancelled".to_string())
}

/// What a file write reports to, and listens to.
pub struct PutProgress {
    /// Bytes sent since the last call.
    pub add: Box<dyn Fn(u64) + Send + Sync>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl PutProgress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The bytes a write sends at a time.
pub(crate) const PUT_CHUNK: usize = 1 << 20;

/// How long a server may take no bytes before a write gives up on it. A plain copy parks on
/// the socket when the server stops reading (a sleeping NAS, Wi-Fi gone with no reset).
pub(crate) const WRITE_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// A file on a server being written front to back ([`RemoteFs::create_writer`]): a few calls
/// to [`Self::write`], then [`Self::finish`], or [`Self::abort`] to give up.
#[allow(clippy::double_must_use)] // async_trait expands to a must_use future
#[async_trait::async_trait]
pub trait RemoteWriter: Send {
    /// Sends the next bytes. A write stopped part way (its future dropped) leaves the file
    /// where nothing more may be written to it: [`Self::abort`] is all that is left.
    async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError>;
    /// Every byte is sent: waits until the server holds the file, then closes it.
    async fn finish(self: Box<Self>) -> Result<(), RemoteError>;
    /// Gives the file up and closes what it holds. What was sent stays on the server, for the
    /// caller to remove.
    async fn abort(self: Box<Self>);
}

/// [`RemoteWriter::write`], failing when the server takes nothing for `stall` and stopping at
/// once when `cancel` is set: nothing would poll a write parked on the socket otherwise.
pub(crate) async fn write_limited(
    w: &mut (dyn RemoteWriter + '_),
    data: &[u8],
    cancel: &std::sync::atomic::AtomicBool,
    stall: std::time::Duration,
    path: &str,
) -> Result<(), RemoteError> {
    tokio::select! {
        r = tokio::time::timeout(stall, w.write(data)) => r.map_err(|_| {
            RemoteError::Io(format!(
                "the server took nothing for {} s while writing {path}",
                stall.as_secs()
            ))
        })?,
        () = until_set(cancel) => Err(RemoteError::Io("cancelled".to_string())),
    }
}

/// Sends the local `file` through `w` and finishes it; on any failure or a cancel the writer is
/// given up (the partial file stays for the caller to remove).
pub(crate) async fn send_file(
    mut file: tokio::fs::File,
    mut w: Box<dyn RemoteWriter + '_>,
    progress: &PutProgress,
    stall: std::time::Duration,
    path: &str,
) -> Result<(), RemoteError> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; PUT_CHUNK];
    loop {
        if progress.cancelled() {
            w.abort().await;
            return Err(RemoteError::Io("cancelled".to_string()));
        }
        let n = match file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                w.abort().await;
                return Err(local_io(e));
            }
        };
        if n == 0 {
            return w.finish().await;
        }
        if let Err(e) = write_limited(&mut *w, &buf[..n], &progress.cancel, stall, path).await {
            w.abort().await;
            return Err(e);
        }
        (progress.add)(n as u64);
    }
}

async fn until_set(flag: &std::sync::atomic::AtomicBool) {
    while !flag.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// A writer over a byte stream to the server (an FTP data connection, an SFTP file): `write`
/// sends every byte, a stream that takes none failing it.
pub(crate) async fn write_all_to<W>(out: &mut W, data: &[u8], what: &str) -> Result<(), RemoteError>
where
    W: tokio::io::AsyncWrite + Unpin + ?Sized,
{
    use tokio::io::AsyncWriteExt;
    let mut sent = 0;
    while sent < data.len() {
        let n = out
            .write(&data[sent..])
            .await
            .map_err(|e| RemoteError::Io(format!("writing {what}: {e}")))?;
        if n == 0 {
            return Err(RemoteError::Io(format!(
                "the server closed the connection while writing {what}"
            )));
        }
        sent += n;
    }
    Ok(())
}

pub(crate) fn local_io(e: std::io::Error) -> RemoteError {
    RemoteError::Io(format!("reading the local copy: {e}"))
}

#[cfg(test)]
/// An in-memory tree standing in for a server in tests. Directories are implied by file paths.
pub(crate) struct MemFs {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Modification times by path; a path with none reports `None`, like a protocol that has none.
    mtimes: Arc<Mutex<BTreeMap<String, i64>>>,
    fail_reads: Arc<AtomicUsize>,
    fail_lists: AtomicUsize,
    lists: AtomicUsize,
    /// Folders made explicitly (an empty one has no file to imply it).
    dirs: Arc<Mutex<BTreeSet<String>>>,
    /// The next writes happen but their reply is lost: the caller sees a dropped connection.
    lose_replies: Arc<AtomicUsize>,
    /// Writes until the server goes away (0: never): that write happens, its reply is lost,
    /// and every call after it fails until [`MemFs::set_down`] brings the server back.
    down_after: Arc<AtomicUsize>,
    down: Arc<std::sync::atomic::AtomicBool>,
    /// Removes no folder, as WebDAV (whose DELETE is recursive) never does.
    keep_dirs: Arc<std::sync::atomic::AtomicBool>,
    /// What `free_space` answers (`None`: the protocol cannot say).
    free: Arc<Mutex<Option<u64>>>,
    /// A byte offset flipped in every file written from now on: a server that stores the
    /// wrong bytes, for read-back tests.
    corrupt_at: Arc<Mutex<Option<u64>>>,
}

#[cfg(test)]
#[allow(deprecated)] // fetch_update is the pre-1.99 spelling of try_update
fn take_one(n: &AtomicUsize) -> bool {
    n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

#[cfg(test)]
impl MemFs {
    pub fn new(files: &[(&str, &[u8])]) -> Self {
        let map = files.iter().map(|(p, b)| (normal(p), b.to_vec())).collect();
        Self {
            files: Arc::new(Mutex::new(map)),
            mtimes: Arc::default(),
            fail_reads: Arc::new(AtomicUsize::new(0)),
            fail_lists: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            dirs: Arc::default(),
            lose_replies: Arc::default(),
            down_after: Arc::default(),
            down: Arc::default(),
            keep_dirs: Arc::default(),
            free: Arc::default(),
            corrupt_at: Arc::default(),
        }
    }

    /// Add (or replace) files after construction; tests sharing one server each add their own.
    #[cfg(test)]
    pub fn add(&self, files: &[(&str, &[u8])]) {
        let mut map = self.files.lock().unwrap();
        for (p, b) in files {
            map.insert(normal(p), b.to_vec());
        }
    }

    /// Gives `path` a modification time (seconds since the epoch).
    #[cfg(test)]
    pub fn set_mtime(&self, path: &str, t: i64) {
        self.mtimes.lock().unwrap().insert(normal(path), t);
    }

    fn mtime_of(&self, p: &str) -> Option<i64> {
        self.mtimes.lock().unwrap().get(p).copied()
    }

    /// A handle on the same tree, for adding files once this one is owned elsewhere.
    #[cfg(test)]
    pub fn share(&self) -> Self {
        Self {
            files: Arc::clone(&self.files),
            mtimes: Arc::clone(&self.mtimes),
            fail_reads: Arc::new(AtomicUsize::new(0)),
            fail_lists: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            dirs: Arc::clone(&self.dirs),
            lose_replies: Arc::clone(&self.lose_replies),
            down_after: Arc::clone(&self.down_after),
            down: Arc::clone(&self.down),
            keep_dirs: Arc::clone(&self.keep_dirs),
            free: Arc::clone(&self.free),
            corrupt_at: Arc::clone(&self.corrupt_at),
        }
    }

    /// What `free_space` answers from now on.
    pub fn set_free(&self, free: Option<u64>) {
        *self.free.lock().unwrap() = free;
    }

    /// Flips the byte at `at` of every file written from now on (`None`: stores what it is
    /// sent).
    pub fn corrupt_writes_at(&self, at: Option<u64>) {
        *self.corrupt_at.lock().unwrap() = at;
    }

    /// Every path the server holds, files and made folders, for tests that check what was left.
    pub fn paths(&self) -> Vec<String> {
        let mut all: Vec<String> = self.files.lock().unwrap().keys().cloned().collect();
        all.extend(self.dirs.lock().unwrap().iter().cloned());
        all.sort();
        all
    }

    /// Folder listings asked for since the last reset.
    pub fn list_count(&self) -> usize {
        self.lists.load(Ordering::SeqCst)
    }

    pub fn reset_list_count(&self) {
        self.lists.store(0, Ordering::SeqCst);
    }

    /// From now on folders are never removed (`remove_dir` is unsupported), as on WebDAV.
    pub fn never_remove_dirs(&self) {
        self.keep_dirs.store(true, Ordering::SeqCst);
    }

    /// The `n`-th write from now takes effect, loses its reply, and takes the server down.
    pub fn go_down_after_writes(&self, n: usize) {
        self.down_after.store(n, Ordering::SeqCst);
    }

    /// Takes the server away (every call fails as a dropped connection) or brings it back.
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Whether the server is away now.
    pub fn is_down(&self) -> bool {
        self.down.load(Ordering::SeqCst)
    }

    fn check_up(&self) -> Result<(), RemoteError> {
        if self.down.load(Ordering::SeqCst) {
            Err(RemoteError::Io("connection reset".into()))
        } else {
            Ok(())
        }
    }

    /// The next `n` writes take effect, but report a dropped connection.
    pub fn lose_next_replies(&self, n: usize) {
        self.lose_replies.store(n, Ordering::SeqCst);
    }

    /// A file's bytes, for tests to check nothing was overwritten.
    pub fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(&normal(path)).cloned()
    }

    /// Runs a write: refused outright, or done with its reply lost, as the test asked.
    fn write<T>(&self, op: impl FnOnce() -> Result<T, RemoteError>) -> Result<T, RemoteError> {
        self.check_up()?;
        let r = op()?;
        if self.down_after.load(Ordering::SeqCst) > 0
            && self.down_after.fetch_sub(1, Ordering::SeqCst) == 1
        {
            self.set_down(true);
        }
        self.check_up()?;
        if take_one(&self.lose_replies) {
            return Err(RemoteError::Io("connection reset".into()));
        }
        Ok(r)
    }

    /// The next `n` listings fail, as a dropped session would.
    pub fn fail_next_lists(&self, n: usize) {
        self.fail_lists.store(n, Ordering::SeqCst);
    }

    #[allow(deprecated)] // fetch_update is the pre-1.99 spelling of try_update
    fn list_fails(&self) -> bool {
        self.fail_lists
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }

    /// The next `n` reads fail, as a dropped connection would.
    pub fn fail_next_reads(&self, n: usize) {
        self.fail_reads.store(n, Ordering::SeqCst);
    }

    /// Direct children of `dir` (sorted by name): files with their size, folders implied.
    fn children(&self, dir: &str) -> Vec<Entry> {
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let files = self.files.lock().unwrap();
        let mut out: BTreeMap<String, Entry> = BTreeMap::new();
        for (p, b) in files.range(prefix.clone()..) {
            let Some(rest) = p.strip_prefix(&prefix) else {
                break;
            };
            match rest.split_once('/') {
                Some((d, _)) => {
                    out.entry(d.to_string()).or_insert(Entry {
                        name: d.to_string(),
                        is_dir: true,
                        size: 0,
                        mtime: None,
                    });
                }
                None => {
                    out.insert(
                        rest.to_string(),
                        Entry {
                            name: rest.to_string(),
                            is_dir: false,
                            size: b.len() as u64,
                            mtime: self.mtime_of(p),
                        },
                    );
                }
            }
        }
        drop(files);
        for d in self.dirs.lock().unwrap().range(prefix.clone()..) {
            let Some(rest) = d.strip_prefix(&prefix) else {
                break;
            };
            let name = rest.split('/').next().unwrap_or(rest);
            if !name.is_empty() {
                out.entry(name.to_string()).or_insert(Entry {
                    name: name.to_string(),
                    is_dir: true,
                    size: 0,
                    mtime: None,
                });
            }
        }
        out.into_values().collect()
    }

    fn is_dir(&self, dir: &str) -> bool {
        let explicit = self.dirs.lock().unwrap().contains(dir);
        dir == "/" || explicit || !self.children(dir).is_empty()
    }

    fn is_file(&self, p: &str) -> bool {
        self.files.lock().unwrap().contains_key(p)
    }

    fn parent_of(p: &str) -> String {
        match p.rsplit_once('/') {
            Some(("", _)) | None => "/".into(),
            Some((d, _)) => d.into(),
        }
    }
}

#[cfg(test)]
fn normal(p: &str) -> String {
    let segs: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    format!("/{}", segs.join("/"))
}

#[cfg(test)]
struct MemFile {
    bytes: Vec<u8>,
    fail_reads: Arc<AtomicUsize>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl RemoteFile for MemFile {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
    #[allow(deprecated)] // fetch_update is the pre-1.99 spelling of try_update
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if self
            .fail_reads
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(RemoteError::Io("connection reset".into()));
        }
        let start = (offset as usize).min(self.bytes.len());
        let end = (offset.saturating_add(len) as usize).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}

/// A file being written on a [`MemFs`]: every write lands at once, so a test sees the
/// partial file grow.
#[cfg(test)]
struct MemWriter<'a> {
    fs: &'a MemFs,
    path: String,
    at: u64,
}

#[cfg(test)]
#[async_trait::async_trait]
impl RemoteWriter for MemWriter<'_> {
    async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError> {
        let (path, at) = (self.path.clone(), self.at);
        let flip = *self.fs.corrupt_at.lock().unwrap();
        self.fs.write(|| {
            let mut files = self.fs.files.lock().unwrap();
            let Some(b) = files.get_mut(&path) else {
                return Err(RemoteError::NotFound(format!("{path} is gone")));
            };
            b.truncate(at as usize);
            b.extend_from_slice(data);
            if let Some(f) = flip.filter(|f| (at..at + data.len() as u64).contains(f)) {
                b[f as usize] ^= 0xFF;
            }
            Ok(())
        })?;
        self.at += data.len() as u64;
        Ok(())
    }

    async fn finish(self: Box<Self>) -> Result<(), RemoteError> {
        self.fs.check_up()
    }

    async fn abort(self: Box<Self>) {}
}

#[cfg(test)]
#[async_trait::async_trait]
impl RemoteFs for MemFs {
    /// As an SFTP or FTP server signs a user in to their home.
    fn login_dir(&self) -> Option<String> {
        Some("/home/me".into())
    }

    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError> {
        self.check_up()?;
        self.lists.fetch_add(1, Ordering::SeqCst);
        if self.list_fails() {
            return Err(RemoteError::Io("connection reset".into()));
        }
        let dir = normal(path);
        if !self.is_dir(&dir) {
            return Err(RemoteError::NotFound(format!("{dir} not found")));
        }
        let all = self.children(&dir);
        let from = cursor.and_then(|c| c.parse::<usize>().ok()).unwrap_or(0);
        let to = (from + PAGE).min(all.len());
        Ok(Page {
            entries: all[from.min(to)..to].to_vec(),
            next_cursor: (to < all.len()).then(|| to.to_string()),
        })
    }

    async fn stat(&self, path: &str) -> Result<Entry, RemoteError> {
        self.check_up()?;
        let p = normal(path);
        if let Some(b) = self.files.lock().unwrap().get(&p) {
            let name = p.rsplit('/').next().unwrap_or("").to_string();
            return Ok(Entry {
                name,
                is_dir: false,
                size: b.len() as u64,
                mtime: self.mtime_of(&p),
            });
        }
        if self.is_dir(&p) {
            let name = p.rsplit('/').next().unwrap_or("").to_string();
            return Ok(Entry {
                name,
                is_dir: true,
                size: 0,
                mtime: None,
            });
        }
        Err(RemoteError::NotFound(format!("{p} not found")))
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        let p = normal(path);
        let bytes = self
            .files
            .lock()
            .unwrap()
            .get(&p)
            .cloned()
            .ok_or_else(|| RemoteError::NotFound(format!("{p} not found")))?;
        Ok(Arc::new(MemFile {
            bytes,
            fail_reads: Arc::clone(&self.fail_reads),
        }))
    }

    async fn walk_until(
        &self,
        path: &str,
        limit: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<(String, Entry)>, RemoteError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(walk_cancelled());
        }
        let dir = normal(path);
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let files = self.files.lock().unwrap();
        Ok(files
            .range(prefix.clone()..)
            .take_while(|(p, _)| p.starts_with(&prefix))
            .take(limit)
            .map(|(p, b)| {
                let rel = p[prefix.len()..].to_string();
                let name = rel.rsplit('/').next().unwrap_or("").to_string();
                (
                    rel,
                    Entry {
                        name,
                        is_dir: false,
                        size: b.len() as u64,
                        mtime: None,
                    },
                )
            })
            .collect())
    }

    async fn create_dir(&self, path: &str) -> Result<(), RemoteError> {
        let p = normal(path);
        self.write(|| {
            if self.is_file(&p) {
                return Err(RemoteError::Exists(format!("{p} is a file")));
            }
            if self.is_dir(&p) {
                return Ok(());
            }
            if !self.is_dir(&Self::parent_of(&p)) {
                return Err(RemoteError::NotFound(format!("no folder for {p}")));
            }
            self.dirs.lock().unwrap().insert(p.clone());
            Ok(())
        })
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        let (from, to) = (normal(from), normal(to));
        self.write(|| {
            if self.is_file(&to) || self.is_dir(&to) {
                return Err(RemoteError::Exists(format!("{to} already exists")));
            }
            if !self.is_dir(&Self::parent_of(&to)) {
                return Err(RemoteError::NotFound(format!("no folder for {to}")));
            }
            let mut files = self.files.lock().unwrap();
            if let Some(b) = files.remove(&from) {
                files.insert(to.clone(), b);
                drop(files);
                let mut m = self.mtimes.lock().unwrap();
                if let Some(t) = m.remove(&from) {
                    m.insert(to.clone(), t);
                }
                // The folder it left stays, as on a real server.
                self.dirs.lock().unwrap().insert(Self::parent_of(&from));
                return Ok(());
            }
            drop(files);
            if from == "/" || !self.is_dir(&from) {
                return Err(RemoteError::NotFound(format!("{from} not found")));
            }
            if to.starts_with(&format!("{from}/")) {
                return Err(RemoteError::Io(format!("{to} is inside {from}")));
            }
            let moved = |k: &String| -> Option<String> {
                if *k == from {
                    Some(to.clone())
                } else {
                    k.strip_prefix(&format!("{from}/"))
                        .map(|rest| format!("{to}/{rest}"))
                }
            };
            let mut files = self.files.lock().unwrap();
            let keys: Vec<String> = files.keys().cloned().collect();
            for k in keys {
                if let Some(n) = moved(&k) {
                    let b = files.remove(&k).unwrap();
                    files.insert(n, b);
                }
            }
            let mut dirs = self.dirs.lock().unwrap();
            let keys: Vec<String> = dirs.iter().cloned().collect();
            for k in keys {
                if let Some(n) = moved(&k) {
                    dirs.remove(&k);
                    dirs.insert(n);
                }
            }
            dirs.insert(to.clone());
            dirs.insert(Self::parent_of(&from));
            Ok(())
        })
    }

    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        let p = normal(path);
        if self.keep_dirs.load(Ordering::SeqCst) {
            return Err(RemoteError::Unsupported(format!("{p} left in place")));
        }
        self.write(|| {
            if p == "/" || !self.is_dir(&p) {
                return Err(RemoteError::NotFound(format!("{p} is not a folder")));
            }
            if !self.children(&p).is_empty() {
                return Err(RemoteError::Io(format!("{p} is not empty")));
            }
            self.dirs.lock().unwrap().remove(&p);
            Ok(())
        })
    }

    async fn create_writer<'a>(
        &'a self,
        path: &str,
        _size: u64,
        from: u64,
    ) -> Result<Box<dyn RemoteWriter + 'a>, RemoteError> {
        let p = normal(path);
        self.write(|| {
            if !self.is_dir(&Self::parent_of(&p)) {
                return Err(RemoteError::NotFound(format!("no folder for {p}")));
            }
            if self.is_dir(&p) {
                return Err(RemoteError::Exists(format!("{p} is a folder")));
            }
            let mut files = self.files.lock().unwrap();
            if from == 0 {
                files.insert(p.clone(), Vec::new());
                return Ok(());
            }
            match files.get_mut(&p) {
                Some(b) if b.len() as u64 >= from => {
                    b.truncate(from as usize);
                    Ok(())
                }
                _ => Err(RemoteError::NotFound(format!(
                    "{p} holds less than {from} bytes"
                ))),
            }
        })?;
        Ok(Box::new(MemWriter {
            fs: self,
            path: p,
            at: from,
        }))
    }

    fn writes_from(&self) -> bool {
        true
    }

    async fn free_space(&self, _path: &str) -> Result<Option<u64>, RemoteError> {
        self.check_up()?;
        Ok(*self.free.lock().unwrap())
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        let p = normal(path);
        self.write(|| match self.files.lock().unwrap().remove(&p) {
            Some(_) => Ok(()),
            None => Err(RemoteError::NotFound(format!("{p} not found"))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_file(bytes: &[u8]) -> std::path::PathBuf {
        let path = crate::remote::store::test_dir().join("src.bin");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn counting() -> (
        PutProgress,
        Arc<AtomicUsize>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let sent = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let added = Arc::clone(&sent);
        (
            PutProgress {
                add: Box::new(move |n| {
                    added.fetch_add(n as usize, std::sync::atomic::Ordering::Relaxed);
                }),
                cancel: Arc::clone(&cancel),
            },
            sent,
            cancel,
        )
    }

    /// A writer over a byte stream, as FTP's and SFTP's are; `ended` says how it ended.
    struct StreamOut {
        out: tokio::io::DuplexStream,
        ended: Arc<Mutex<&'static str>>,
    }

    #[async_trait::async_trait]
    impl RemoteWriter for StreamOut {
        async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError> {
            write_all_to(&mut self.out, data, "/a").await
        }
        async fn finish(self: Box<Self>) -> Result<(), RemoteError> {
            *self.ended.lock().unwrap() = "finished";
            Ok(())
        }
        async fn abort(self: Box<Self>) {
            *self.ended.lock().unwrap() = "aborted";
        }
    }

    type Ended = Arc<Mutex<&'static str>>;

    fn stream_out(cap: usize) -> (Box<StreamOut>, tokio::io::DuplexStream, Ended) {
        let (out, server) = tokio::io::duplex(cap);
        let ended = Arc::new(Mutex::new(""));
        (
            Box::new(StreamOut {
                out,
                ended: Arc::clone(&ended),
            }),
            server,
            ended,
        )
    }

    #[tokio::test]
    async fn a_write_delivers_every_byte_and_counts_them() {
        use tokio::io::AsyncReadExt;
        let body: Vec<u8> = (0..3 * PUT_CHUNK + 17).map(|i| (i % 251) as u8).collect();
        let path = local_file(&body);
        let (progress, sent, _) = counting();
        let (w, mut server, ended) = stream_out(64 * 1024);
        let reader = tokio::spawn(async move {
            let mut got = Vec::new();
            server.read_to_end(&mut got).await.unwrap();
            got
        });
        let file = tokio::fs::File::open(&path).await.unwrap();
        send_file(file, w, &progress, std::time::Duration::from_secs(5), "/a")
            .await
            .unwrap();
        assert_eq!(reader.await.unwrap(), body);
        assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), body.len());
        assert_eq!(*ended.lock().unwrap(), "finished");
    }

    /// The server stops reading and says nothing: the write ends with a reason of its own and
    /// the writer is given up. A plain copy parked there for good, holding the session.
    #[tokio::test]
    async fn a_server_that_stops_reading_fails_the_write() {
        let path = local_file(&vec![1u8; 4 * PUT_CHUNK]);
        let (progress, _, _) = counting();
        let (w, _server, ended) = stream_out(1024);
        let file = tokio::fs::File::open(&path).await.unwrap();
        let e = send_file(
            file,
            w,
            &progress,
            std::time::Duration::from_millis(50),
            "/a",
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("took nothing"), "{e}");
        assert_eq!(*ended.lock().unwrap(), "aborted");
    }

    /// Cancel reaches a write that is parked on the socket: nothing polls the file then.
    #[tokio::test]
    async fn cancel_stops_a_write_stuck_on_the_socket() {
        let path = local_file(&vec![1u8; 4 * PUT_CHUNK]);
        let (progress, _, cancel) = counting();
        let (w, _server, ended) = stream_out(1024);
        let file = tokio::fs::File::open(&path).await.unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let e = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            send_file(
                file,
                w,
                &progress,
                std::time::Duration::from_secs(600),
                "/a",
            ),
        )
        .await
        .expect("the cancel must end the write")
        .unwrap_err();
        assert_eq!(e.to_string(), "cancelled");
        assert_eq!(*ended.lock().unwrap(), "aborted");
    }

    /// MemFs writes through the one writer path: put_file, a write from a byte past the start,
    /// and one into a missing folder.
    #[tokio::test]
    async fn memfs_writes_whole_and_from_a_byte() {
        let fs = MemFs::new(&[("/w/keep", b"x")]);
        let path = local_file(b"0123456789");
        let (progress, sent, _) = counting();
        fs.put_file("/w/a", &path, &progress).await.unwrap();
        assert_eq!(fs.bytes("/w/a").unwrap(), b"0123456789");
        assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), 10);
        let mut w = fs.create_writer("/w/a", 10, 4).await.unwrap();
        w.write(b"ABCDEF").await.unwrap();
        w.finish().await.unwrap();
        assert_eq!(fs.bytes("/w/a").unwrap(), b"0123ABCDEF");
        assert!(matches!(
            fs.create_writer("/nope/a", 1, 0).await.err(),
            Some(RemoteError::NotFound(_))
        ));
        assert!(fs.create_writer("/w/a", 100, 50).await.is_err());
    }

    #[tokio::test]
    async fn memfs_lists_pages_stats_reads_and_walks() {
        let many: Vec<(String, Vec<u8>)> = (0..450)
            .map(|i| (format!("/big/f{i:03}"), vec![1]))
            .collect();
        let mut files: Vec<(&str, &[u8])> = many
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        files.push(("/g/sce_sys/param.json", b"{}"));
        files.push(("/g/eboot.bin", b"0123456789"));
        let fs = MemFs::new(&files);

        let p1 = fs.list("/big", None).await.unwrap();
        assert_eq!(p1.entries.len(), PAGE);
        let p2 = fs.list("/big", p1.next_cursor).await.unwrap();
        let p3 = fs.list("/big", p2.next_cursor).await.unwrap();
        assert_eq!(p3.entries.len(), 50);
        assert!(p3.next_cursor.is_none());

        let root = fs.list("/", None).await.unwrap();
        let names: Vec<_> = root
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir))
            .collect();
        assert_eq!(names, [("big", true), ("g", true)]);

        assert!(fs.stat("/g").await.unwrap().is_dir);
        assert_eq!(fs.stat("/g/eboot.bin").await.unwrap().size, 10);
        let f = fs.open("/g/eboot.bin").await.unwrap();
        assert_eq!(f.size(), 10);
        assert_eq!(f.read_at(3, 4).await.unwrap(), b"3456");
        assert_eq!(f.read_at(8, 10).await.unwrap(), b"89");

        let walked = fs.walk("/g", 100).await.unwrap();
        let names: Vec<_> = walked.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["eboot.bin", "sce_sys/param.json"]);
        assert_eq!(fs.walk("/big", 10).await.unwrap().len(), 10);

        assert!(matches!(
            fs.stat("/nope").await,
            Err(RemoteError::NotFound(_))
        ));
        assert!(matches!(fs.open("/g").await, Err(RemoteError::NotFound(_))));
    }

    #[tokio::test]
    async fn memfs_meets_the_contract() {
        let fs = MemFs::new(&[
            ("/g/a.bin", b"0123456789abcdefXYZ"),
            ("/g/sub/b", b"x"),
            ("/w/a", b"aaa"),
            ("/w/b", b"bbbbb"),
        ]);
        crate::remote::contract::check(&fs, "/g").await;
        crate::remote::contract::check_writes(&fs, "/w").await;
    }

    #[tokio::test]
    async fn memfs_can_fail_reads_for_retry_tests() {
        let fs = MemFs::new(&[("/a", b"xy")]);
        let f = fs.open("/a").await.unwrap();
        fs.fail_next_reads(1);
        assert!(f.read_at(0, 2).await.is_err());
        assert_eq!(f.read_at(0, 2).await.unwrap(), b"xy");
    }
}
