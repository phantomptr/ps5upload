//! Files on the user's own servers (SMB, FTP, FTPS, SFTP), read through one interface.
//!
//! A pick from a server is a `remote://<connection-id>/<path>` string (see [`path`]). The engine
//! resolves it against the saved connections and reads it through [`RemoteFs`]: listing for the
//! in-app browser, positioned reads for streaming installs and uploads, and whole-file copies for
//! the jobs that need the bytes on this computer.

#![allow(dead_code)] // Consumers arrive in later steps of the remote-sources plan.

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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
    /// The server's identity (SFTP host key, FTPS certificate) is not the one the user accepted.
    #[error("Sign-in failed: {}", if *changed { "the server's key has changed since you last connected" } else { "unknown server key — accept it to connect" })]
    HostKey { fingerprint: String, changed: bool },
}

#[async_trait::async_trait]
pub trait RemoteFile: Send + Sync {
    fn size(&self) -> u64;
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError>;
}

#[async_trait::async_trait]
pub trait RemoteFs: Send + Sync {
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError>;
    async fn stat(&self, path: &str) -> Result<Entry, RemoteError>;
    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError>;
    /// Every regular file under `path`, as (path relative to `path`, entry), capped at `limit`.
    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError>;
}

/// An in-memory tree standing in for a server in tests. Directories are implied by file paths.
pub(crate) struct MemFs {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    fail_reads: Arc<AtomicUsize>,
}

impl MemFs {
    pub fn new(files: &[(&str, &[u8])]) -> Self {
        let map = files.iter().map(|(p, b)| (normal(p), b.to_vec())).collect();
        Self {
            files: Arc::new(Mutex::new(map)),
            fail_reads: Arc::new(AtomicUsize::new(0)),
        }
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
                            mtime: None,
                        },
                    );
                }
            }
        }
        out.into_values().collect()
    }

    fn is_dir(&self, dir: &str) -> bool {
        dir == "/" || !self.children(dir).is_empty()
    }
}

fn normal(p: &str) -> String {
    let segs: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    format!("/{}", segs.join("/"))
}

struct MemFile {
    bytes: Vec<u8>,
    fail_reads: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RemoteFile for MemFile {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
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

#[async_trait::async_trait]
impl RemoteFs for MemFs {
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError> {
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
        let p = normal(path);
        if let Some(b) = self.files.lock().unwrap().get(&p) {
            let name = p.rsplit('/').next().unwrap_or("").to_string();
            return Ok(Entry {
                name,
                is_dir: false,
                size: b.len() as u64,
                mtime: None,
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
        let fs = MemFs::new(&[("/g/a.bin", b"0123456789abcdefXYZ"), ("/g/sub/b", b"x")]);
        crate::remote::contract::check(&fs, "/g").await;
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
