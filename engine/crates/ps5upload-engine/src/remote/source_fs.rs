//! The upload core's [`SourceFs`] over a saved server, so a folder on a NAS uploads to the
//! console without a copy on this computer.
//!
//! Paths are the server path as a `PathBuf` ("/games/x"). Everything here blocks on the runtime:
//! the transfer runs in `spawn_blocking`, never on a runtime worker. Reads go through a
//! read-ahead buffer, because the core reads a shard at a time in small calls and one server
//! round trip per call would be slow.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ps5upload_core::source_fs::{ReadSeek, SourceFs, SourceMeta};

use super::pool::{Backoff, Pool};
use super::store::Store;
use super::{RemoteError, RemoteFile, RemoteFs};

/// How far ahead each server read goes.
const READ_AHEAD: u64 = 8 * 1024 * 1024;

pub struct RemoteSourceFs {
    handle: tokio::runtime::Handle,
    fs: Arc<dyn RemoteFs>,
    pool: Arc<Pool>,
    store: Arc<Store>,
    id: String,
    /// What listings already said about each path, so the per-file stats that follow a walk
    /// cost no round trip.
    known: std::sync::Mutex<std::collections::HashMap<String, (u64, bool)>>,
}

impl std::fmt::Debug for RemoteSourceFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSourceFs")
            .field("connection", &self.id)
            .finish_non_exhaustive()
    }
}

impl RemoteSourceFs {
    pub async fn new(
        pool: Arc<Pool>,
        store: Arc<Store>,
        connection_id: &str,
    ) -> Result<Self, RemoteError> {
        let fs = pool.fs(&store, connection_id).await?;
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            fs,
            pool,
            store,
            id: connection_id.to_string(),
            known: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The source file system and server path for a `remote://` upload source.
    pub async fn for_path(remote_path: &str) -> Result<(Arc<Self>, PathBuf), RemoteError> {
        let p = super::path::parse(remote_path)?;
        let r = super::pool::global()?;
        let fs = Self::new(Arc::clone(&r.pool), Arc::clone(&r.store), &p.connection_id).await?;
        Ok((Arc::new(fs), PathBuf::from(p.path)))
    }
}

/// A `PathBuf` holding a server path, back to the `/`-separated string the server takes.
fn server_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn io_err(e: RemoteError) -> std::io::Error {
    let kind = match e {
        RemoteError::NotFound(_) => std::io::ErrorKind::NotFound,
        RemoteError::Auth(_) => std::io::ErrorKind::PermissionDenied,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, e.to_string())
}

impl SourceFs for RemoteSourceFs {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>> {
        let path = server_path(p);
        let file = self
            .handle
            .block_on(self.pool.with_fs(&self.store, &self.id, |fs| {
                let path = path.clone();
                async move { fs.open(&path).await }
            }))
            .map_err(io_err)?;
        let file = super::pool::retrying(
            Arc::clone(&self.pool),
            Arc::clone(&self.store),
            self.id.clone(),
            path,
            file,
            Backoff::standard(),
        );
        Ok(Box::new(Reader {
            handle: self.handle.clone(),
            size: file.size(),
            file,
            pos: 0,
            buf: Vec::new(),
            buf_start: 0,
        }))
    }

    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta> {
        let path = server_path(p);
        let seen = self
            .known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .copied();
        if let Some((len, is_dir)) = seen {
            return Ok(SourceMeta {
                len,
                is_dir,
                is_file: !is_dir,
            });
        }
        let e = self
            .handle
            .block_on(self.pool.with_fs(&self.store, &self.id, |fs| {
                let path = path.clone();
                async move { fs.stat(&path).await }
            }))
            .map_err(io_err)?;
        Ok(SourceMeta {
            len: e.size,
            is_dir: e.is_dir,
            is_file: !e.is_dir,
        })
    }

    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
        let dir = server_path(p);
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .handle
                .block_on(self.pool.with_fs(&self.store, &self.id, |fs| {
                    let (dir, cursor) = (dir.clone(), cursor.clone());
                    async move { fs.list(&dir, cursor).await }
                }))
                .map_err(io_err)?;
            for e in page.entries {
                let child = super::path::join(&dir, &e.name).map_err(|err| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
                })?;
                self.known
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(child.clone(), (e.size, e.is_dir));
                out.push((PathBuf::from(child), e.is_dir));
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }
}

/// A server file read sequentially (with seeks) through a read-ahead buffer.
struct Reader {
    handle: tokio::runtime::Handle,
    file: Arc<dyn RemoteFile>,
    size: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_start: u64,
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        let buf_end = self.buf_start + self.buf.len() as u64;
        if self.pos < self.buf_start || self.pos >= buf_end {
            let len = READ_AHEAD.min(self.size - self.pos);
            self.buf = self
                .handle
                .block_on(self.file.read_at(self.pos, len))
                .map_err(io_err)?;
            self.buf_start = self.pos;
            if self.buf.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("the server returned no data at {}", self.pos),
                ));
            }
        }
        let at = (self.pos - self.buf_start) as usize;
        let n = out.len().min(self.buf.len() - at);
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => self.size as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if next < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::pool::testing::remote_with_shared;
    use crate::remote::store::{conn, Protocol, Secret};
    use crate::remote::MemFs;

    async fn setup(files: &[(&str, &[u8])]) -> RemoteSourceFs {
        let r = remote_with_shared(Arc::new(MemFs::new(files)), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        RemoteSourceFs::new(Arc::clone(&r.pool), Arc::clone(&r.store), &id)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stats_lists_and_reads_like_a_disk() {
        let fs = setup(&[
            ("/g/eboot.bin", b"0123456789"),
            ("/g/sce_sys/param.json", b"{}"),
        ])
        .await;
        tokio::task::spawn_blocking(move || {
            let m = fs.metadata(Path::new("/g/eboot.bin")).unwrap();
            assert!(m.is_file && !m.is_dir);
            assert_eq!(m.len, 10);
            assert!(fs.metadata(Path::new("/g")).unwrap().is_dir);
            let mut kids = fs.read_dir(Path::new("/g")).unwrap();
            kids.sort();
            assert_eq!(
                kids,
                [
                    (PathBuf::from("/g/eboot.bin"), false),
                    (PathBuf::from("/g/sce_sys"), true)
                ]
            );
            let mut f = fs.open(Path::new("/g/eboot.bin")).unwrap();
            f.seek(SeekFrom::Start(3)).unwrap();
            let mut rest = Vec::new();
            f.read_to_end(&mut rest).unwrap();
            assert_eq!(rest, b"3456789");
            assert!(fs.open(Path::new("/g/nope")).is_err());
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_big_file_read_in_small_calls_matches_the_source() {
        let data: Vec<u8> = (0..(20 * 1024 * 1024)).map(|i| (i % 253) as u8).collect();
        let fs = setup(&[("/big.bin", &data)]).await;
        let got = tokio::task::spawn_blocking(move || {
            let mut f = fs.open(Path::new("/big.bin")).unwrap();
            let mut out = Vec::new();
            let mut buf = vec![0u8; 1024 * 1024 + 7];
            loop {
                let n = f.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            out
        })
        .await
        .unwrap();
        assert_eq!(got.len(), data.len());
        assert!(got == data);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_folder_upload_plans_from_the_listing() {
        let fs = setup(&[
            ("/g/eboot.bin", b"0123456789"),
            ("/g/sce_sys/param.json", b"{}"),
            ("/g/.DS_Store", b"junk"),
        ])
        .await;
        let (total, files) = tokio::task::spawn_blocking(move || {
            crate::walk_plan_with(&fs, Path::new("/g"), &[".DS_Store".to_string()])
        })
        .await
        .unwrap()
        .unwrap();
        let names: Vec<_> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(names, ["eboot.bin", "sce_sys/param.json"]);
        assert_eq!(total, 12);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_folder_that_cannot_be_listed_fails_the_upload_plan() {
        let mem = Arc::new(MemFs::new(&[("/g/a.bin", b"x"), ("/g/sub/b.bin", b"y")]));
        let r = remote_with_shared(Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = RemoteSourceFs::new(Arc::clone(&r.pool), Arc::clone(&r.store), &id)
            .await
            .unwrap();
        mem.fail_next_lists(50);
        let plan =
            tokio::task::spawn_blocking(move || crate::walk_plan_with(&fs, Path::new("/g"), &[]))
                .await
                .unwrap();
        assert!(
            plan.is_err(),
            "a listing that failed must not become a smaller upload"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_session_mid_upload_signs_in_again() {
        let mem = Arc::new(MemFs::new(&[("/g/a.bin", b"x"), ("/g/sub/b.bin", b"y")]));
        let r = remote_with_shared(Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = RemoteSourceFs::new(Arc::clone(&r.pool), Arc::clone(&r.store), &id)
            .await
            .unwrap();
        mem.fail_next_lists(1);
        let plan =
            tokio::task::spawn_blocking(move || crate::walk_plan_with(&fs, Path::new("/g"), &[]))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(plan.1.len(), 2);
    }
}
