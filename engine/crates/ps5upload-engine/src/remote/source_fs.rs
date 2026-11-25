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

/// How far ahead a server read goes at most.
const READ_AHEAD: u64 = 8 * 1024 * 1024;
/// How far ahead the first read of a file (and a read after a jump) goes. A header probe (the
/// Collection reading a package's identity) then costs this, not 8 MiB; a sequential reader
/// doubles it on every refill, so an upload is at the full read-ahead within a few reads.
const FIRST_READ: u64 = 256 * 1024;

/// A listing's word on one path: (size, is_dir, mtime).
type Known = (u64, bool, Option<i64>);

pub struct RemoteSourceFs {
    handle: tokio::runtime::Handle,
    /// Held so the pooled session the listings came from stays open.
    _fs: Arc<dyn RemoteFs>,
    pool: Arc<Pool>,
    store: Arc<Store>,
    id: String,
    /// The server is SMB: a name Samba made up stops a walk (see `entries`).
    smb: bool,
    /// What listings already said about each path, so the per-file stats that follow a walk
    /// cost no round trip.
    known: std::sync::Mutex<std::collections::HashMap<String, Known>>,
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
        let smb = store
            .get(connection_id)
            .is_some_and(|(c, _)| c.protocol == super::store::Protocol::Smb);
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            _fs: fs,
            pool,
            store,
            id: connection_id.to_string(),
            smb,
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

/// A `PathBuf` holding a server path, back to the `/`-separated string the server takes. Only
/// on Windows is a `\\` a `PathBuf` separator; elsewhere it is part of a name and stays.
fn server_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
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
        // The sizes an upload plans with are the listing's (`metadata`, from `known`). A file
        // that opens at another size is not the file that was planned: a link whose listed
        // length is the link's own (FTP, and SFTP for a link to something other than a file),
        // or a file changed since. Sent anyway, it arrived cut to the listed length and the
        // upload ended as done.
        let listed = self
            .known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .copied();
        if let Some((listed, false, _)) = listed {
            if listed != file.size() {
                return Err(std::io::Error::other(format!(
                    "{path} is {} bytes, but its folder listed it as {listed} (a link, or a \
                     file that changed since): not sent, so it is not left cut short",
                    file.size()
                )));
            }
        }
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
            ahead: FIRST_READ,
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
        if let Some((len, is_dir, _)) = seen {
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

    /// The server's modification time: from the listing that named the path, else one stat.
    /// `None` when the protocol reports none (then AVA1 compares content instead).
    fn mtime(&self, p: &Path) -> Option<u64> {
        let path = server_path(p);
        let seen = self
            .known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .copied();
        let t = match seen {
            Some((_, _, t)) => t,
            None => {
                self.handle
                    .block_on(self.pool.with_fs(&self.store, &self.id, |fs| {
                        let path = path.clone();
                        async move { fs.stat(&path).await }
                    }))
                    .ok()?
                    .mtime
            }
        };
        t.filter(|t| *t > 0).map(|t| t as u64)
    }

    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
        let dir = server_path(p);
        self.entries(&dir)?
            .into_iter()
            .map(|e| {
                let child = super::path::join(&dir, &e.name).map_err(|err| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
                })?;
                Ok((PathBuf::from(child), e.is_dir))
            })
            .collect()
    }
}

impl RemoteSourceFs {
    /// Every entry of the server folder `dir` (a server path), names exactly as the server
    /// gave them. Remembered for the stats that follow.
    ///
    /// On an SMB server, a folder holding names Samba made up (see
    /// [`super::smb_fs::looks_renamed_by_samba`]) is an error: copied or converted, the game gets
    /// those files under the wrong names. Browsing the folder lists them as they are.
    pub fn entries(&self, dir: &str) -> std::io::Result<Vec<super::Entry>> {
        let out = self.listed(dir)?;
        if self.smb {
            let renamed: Vec<&str> = out
                .iter()
                .map(|e| e.name.as_str())
                .filter(|n| super::smb_fs::looks_renamed_by_samba(n))
                .collect();
            if let Some(first) = renamed.first() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "the server renamed {} file(s) in {dir} (for example \"{first}\"): their \
                         real names hold characters Windows file sharing cannot, such as : ? * or \
                         a trailing dot, and the real names cannot be read back over SMB. Copied \
                         like this the game would get the wrong file names. Add this server over \
                         SFTP or FTP instead, which keep the real names.",
                        renamed.len()
                    ),
                ));
            }
        }
        Ok(out)
    }

    fn listed(&self, dir: &str) -> std::io::Result<Vec<super::Entry>> {
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .handle
                .block_on(self.pool.with_fs(&self.store, &self.id, |fs| {
                    let (dir, cursor) = (dir.to_string(), cursor.clone());
                    async move { fs.list(&dir, cursor).await }
                }))
                .map_err(io_err)?;
            for e in page.entries {
                if let Ok(child) = super::path::join(dir, &e.name) {
                    self.known
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(child, (e.size, e.is_dir, e.mtime));
                }
                out.push(e);
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
    /// The next refill's size: doubles while reads continue where the buffer ended.
    ahead: u64,
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        let buf_end = self.buf_start + self.buf.len() as u64;
        if self.pos < self.buf_start || self.pos >= buf_end {
            self.ahead = if !self.buf.is_empty() && self.pos == buf_end {
                (self.ahead * 2).min(READ_AHEAD)
            } else {
                FIRST_READ
            };
            let len = self.ahead.max(out.len() as u64).min(self.size - self.pos);
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
        setup_with(MemFs::new(files)).await
    }

    async fn setup_with(mem: MemFs) -> RemoteSourceFs {
        let r = remote_with_shared(Arc::new(mem), None);
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
    async fn a_backend_with_mtimes_reports_them_and_one_without_reports_none() {
        let mem = MemFs::new(&[("/g/a.bin", b"aa"), ("/g/b.bin", b"bb")]);
        mem.set_mtime("/g/a.bin", 1_700_000_123);
        let fs = setup_with(mem).await;
        tokio::task::spawn_blocking(move || {
            // Before any listing: one stat.
            assert_eq!(fs.mtime(Path::new("/g/a.bin")), Some(1_700_000_123));
            assert_eq!(fs.mtime(Path::new("/g/b.bin")), None, "unknown, not 0");
            // After a listing: answered from what it said.
            fs.read_dir(Path::new("/g")).unwrap();
            assert_eq!(fs.mtime(Path::new("/g/a.bin")), Some(1_700_000_123));
            assert_eq!(fs.mtime(Path::new("/g/b.bin")), None);
        })
        .await
        .unwrap();
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

    /// An upload plans with the sizes a listing gave. A file that then opens at another size
    /// (a link listed with its own length, a file changed since) is refused: it used to be
    /// sent cut to the listed length, and the upload ended as done.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_that_opens_at_another_size_than_listed_is_refused() {
        let mem = MemFs::new(&[("/g/link.bin", b"1234567"), ("/g/same.bin", b"abc")]);
        let server = mem.share();
        let fs = setup_with(mem).await;
        tokio::task::spawn_blocking(move || {
            fs.read_dir(Path::new("/g")).unwrap();
            assert_eq!(fs.metadata(Path::new("/g/link.bin")).unwrap().len, 7);
            // What the name really opens is longer than its listing said.
            server.add(&[("/g/link.bin", &[9u8; 100])]);
            let e = fs.open(Path::new("/g/link.bin")).err().expect("refused");
            assert!(e.to_string().contains("listed it as 7"), "{e}");
            // A file as listed opens, and one never listed has nothing to disagree with.
            assert!(fs.open(Path::new("/g/same.bin")).is_ok());
            server.add(&[("/h/new.bin", b"xy")]);
            assert!(fs.open(Path::new("/h/new.bin")).is_ok());
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
    async fn a_folder_holding_names_samba_made_up_fails_the_walk() {
        // `sce_sys:param.json` on a Linux NAS, as Samba lists it.
        let fs = setup(&[("/g/eboot.bin", b"e"), ("/g/S3UXDG~T", b"p")]).await;
        let err = tokio::task::spawn_blocking(move || fs.read_dir(Path::new("/g")))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("S3UXDG~T"), "{err}");
        assert!(err.to_string().contains("SFTP or FTP"), "{err}");
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
