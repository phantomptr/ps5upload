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
    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError>;

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

    // ── Writing files: downloads from a console to a server (8.0.0) ──

    /// Writes the local file `src` to `path`, creating or truncating it. `progress` hears the
    /// bytes as they go; a set `cancel` stops the write (the partial file is left for the
    /// caller to remove).
    async fn put_file(
        &self,
        path: &str,
        _src: &std::path::Path,
        _progress: &PutProgress,
    ) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot write {path}"
        )))
    }

    /// Removes one file.
    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "this server cannot remove {path}"
        )))
    }
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

/// A local file read for a write, counting what goes and stopping when cancelled.
pub(crate) struct CountingReader<'a> {
    pub file: std::fs::File,
    pub progress: &'a PutProgress,
}

impl std::io::Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.progress.cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = self.file.read(buf)?;
        (self.progress.add)(n as u64);
        Ok(n)
    }
}

/// The same for the async writers.
pub(crate) struct AsyncCountingReader<'a> {
    pub file: tokio::fs::File,
    pub progress: &'a PutProgress,
}

impl tokio::io::AsyncRead for AsyncCountingReader<'_> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.progress.cancelled() {
            return std::task::Poll::Ready(Err(std::io::Error::other("cancelled")));
        }
        let before = buf.filled().len();
        let r = std::pin::Pin::new(&mut self.file).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &r {
            (self.progress.add)((buf.filled().len() - before) as u64);
        }
        r
    }
}

fn local_io(e: std::io::Error) -> RemoteError {
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
            dirs: Arc::default(),
            lose_replies: Arc::default(),
            down_after: Arc::default(),
            down: Arc::default(),
            keep_dirs: Arc::default(),
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
            dirs: Arc::clone(&self.dirs),
            lose_replies: Arc::clone(&self.lose_replies),
            down_after: Arc::clone(&self.down_after),
            down: Arc::clone(&self.down),
            keep_dirs: Arc::clone(&self.keep_dirs),
        }
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

#[cfg(test)]
#[async_trait::async_trait]
impl RemoteFs for MemFs {
    /// As an SFTP or FTP server signs a user in to their home.
    fn login_dir(&self) -> Option<String> {
        Some("/home/me".into())
    }

    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError> {
        self.check_up()?;
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

    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError> {
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

    async fn put_file(
        &self,
        path: &str,
        src: &std::path::Path,
        progress: &PutProgress,
    ) -> Result<(), RemoteError> {
        let p = normal(path);
        if progress.cancelled() {
            return Err(RemoteError::Io("cancelled".into()));
        }
        let bytes = std::fs::read(src).map_err(local_io)?;
        self.write(|| {
            if !self.is_dir(&Self::parent_of(&p)) {
                return Err(RemoteError::NotFound(format!("no folder for {p}")));
            }
            (progress.add)(bytes.len() as u64);
            self.files.lock().unwrap().insert(p.clone(), bytes.clone());
            Ok(())
        })
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
