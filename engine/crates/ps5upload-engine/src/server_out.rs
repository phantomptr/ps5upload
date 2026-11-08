//! Convert's output straight to a saved server (a NAS share over SMB or NFS, FTP, SFTP or
//! WebDAV): the image or folder never exists on this computer.
//!
//! How the bytes go. The writers run on the job's thread and write forward only (see
//! `image_build::to_console`); [`Upload`] is the `Write + Seek` they write into. It hands 1 MiB
//! pieces down a bounded pipe to a thread of its own that feeds them to the server through
//! [`RemoteFs::create_writer`], the one write path every protocol has, so building and sending
//! overlap and the memory used stays bounded. A server that stops taking bytes fails the write
//! after [`crate::remote::WRITE_STALL`]; a cancel stops it at once.
//!
//! What is read back goes through a fresh view of the server ([`ServerDest::files`]): nothing it
//! answers comes from a listing made before the write.

use std::io::{self, Seek, SeekFrom, Write};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

use ps5upload_fpkg::remote_source::RemoteFiles;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

use crate::remote::pool::Remote;
use crate::remote::{RemoteError, RemoteFs};

/// What goes down the pipe at a time.
const PIECE: usize = 1 << 20;
/// Pieces in flight between the writer and the server.
const PIPE: usize = 8;

/// A folder on a saved server that Convert writes into.
pub(crate) struct ServerDest {
    /// The saved connection.
    pub id: String,
    /// The folder on it, a server path ("/games/ps5").
    pub dir: String,
    /// The connection's name, for messages ("the server NAS").
    pub label: String,
    /// A share (SMB): names Windows cannot hold are refused before anything is written.
    pub windows_names: bool,
    remote: Arc<Remote>,
    handle: tokio::runtime::Handle,
}

impl ServerDest {
    /// The connection `id` from the engine's saved ones, `dir` on it. Fails when there is no
    /// such connection; nothing is signed in to yet.
    pub(crate) fn new(id: &str, dir: &str) -> Result<Self, RemoteError> {
        let remote = crate::remote::pool::global()?;
        Self::with(remote, id, dir, tokio::runtime::Handle::current())
    }

    pub(crate) fn with(
        remote: Arc<Remote>,
        id: &str,
        dir: &str,
        handle: tokio::runtime::Handle,
    ) -> Result<Self, RemoteError> {
        let Some((conn, _)) = remote.store.get(id) else {
            return Err(RemoteError::UnknownConnection(id.to_string()));
        };
        let dir = crate::remote::path::parse(&format!("remote://{id}/{}", dir.trim()))?.path;
        Ok(Self {
            id: id.to_string(),
            dir,
            label: format!("the server {}", conn.name),
            windows_names: conn.protocol == crate::remote::store::Protocol::Smb,
            remote,
            handle,
        })
    }

    /// The folder's `name`, a server path.
    pub(crate) fn path_of(&self, name: &str) -> String {
        format!("{}/{name}", self.dir.trim_end_matches('/'))
    }

    /// How a job names `path` on the server: `remote://<id><path>`.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("remote://{}{path}", self.id)
    }

    /// Runs `f` on the engine's runtime from this (blocking) thread.
    pub(crate) fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.handle.block_on(f)
    }

    /// The signed-in session (signing in again after [`Self::reconnect`]).
    pub(crate) fn fs(&self) -> Result<Arc<dyn RemoteFs>, RemoteError> {
        self.block_on(self.remote.pool.fs(&self.remote.store, &self.id))
    }

    /// Forgets the session, so the next [`Self::fs`] signs in afresh: after a dropped
    /// connection.
    pub(crate) fn reconnect(&self) {
        self.remote.pool.invalidate(&self.id);
    }

    /// A fresh view of the server's files for reading back: nothing it says comes from a
    /// listing made before.
    pub(crate) fn files(&self) -> Result<Arc<dyn RemoteFiles>, RemoteError> {
        let fs = self.block_on(crate::remote::source_fs::RemoteSourceFs::new(
            Arc::clone(&self.remote.pool),
            Arc::clone(&self.remote.store),
            &self.id,
        ))?;
        Ok(crate::convert_source::server_files(
            Arc::new(fs),
            self.label.clone(),
        ))
    }

    /// Makes [`Self::dir`] and every missing folder above it.
    pub(crate) fn make_dir(&self, fs: &dyn RemoteFs) -> Result<(), RemoteError> {
        self.block_on(make_dirs(fs, &self.dir))
    }

    /// Whether `path` is there (a file or a folder). A failure to ask is an error, never "no".
    pub(crate) fn exists(&self, fs: &dyn RemoteFs, path: &str) -> Result<bool, RemoteError> {
        match self.block_on(fs.stat(path)) {
            Ok(_) => Ok(true),
            Err(RemoteError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The size of the file at `path`; `None` when there is none.
    pub(crate) fn file_size(
        &self,
        fs: &dyn RemoteFs,
        path: &str,
    ) -> Result<Option<u64>, RemoteError> {
        match self.block_on(fs.stat(path)) {
            Ok(e) if !e.is_dir => Ok(Some(e.size)),
            Ok(_) => Err(RemoteError::Exists(format!("{path} is a folder"))),
            Err(RemoteError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The up-front space check: refused only when the server itself says `need` bytes cannot
    /// fit. A protocol that cannot say (FTP, WebDAV), or a server that does not answer, is not
    /// refused: the write fails with the server's own reason if it fills.
    pub(crate) fn room_check(&self, fs: &dyn RemoteFs, need: u64) -> Result<(), String> {
        match self.block_on(fs.free_space(&self.dir)) {
            Ok(Some(free)) if need > free => Err(format!(
                "only {:.1} GiB free in {} on {}, and this needs {:.1} GiB: make room there or \
                 choose another folder",
                free as f64 / (1u64 << 30) as f64,
                self.dir,
                self.label,
                need as f64 / (1u64 << 30) as f64,
            )),
            Ok(_) => Ok(()),
            Err(e) => {
                crate::log_at!(
                    Info,
                    "convert",
                    "to server: free space of {} unknown ({e}); going ahead",
                    self.dir
                );
                Ok(())
            }
        }
    }

    /// Removes the file at `path`, saying what happened in a phrase for the job's message.
    pub(crate) fn remove(&self, fs: &dyn RemoteFs, path: &str) -> String {
        match self.block_on(fs.remove_file(path)) {
            Ok(()) | Err(RemoteError::NotFound(_)) => format!("it was removed from {}", self.label),
            Err(e) => format!(
                "removing it from {} failed too ({e}): delete {path} by hand",
                self.label
            ),
        }
    }
}

/// Makes `dir` and every missing parent on the server.
pub(crate) async fn make_dirs(fs: &dyn RemoteFs, dir: &str) -> Result<(), RemoteError> {
    let mut at = String::new();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        at = format!("{at}/{part}");
        fs.create_dir(&at).await?;
    }
    Ok(())
}

/// The message of a server error, with the hint the remote browser shows for it.
pub(crate) fn said(e: &RemoteError) -> String {
    crate::remote::api::error_with_hint(e)
}

/// What the sending thread hears.
enum Msg {
    Data(Vec<u8>),
    End,
}

/// A file being written on the server from this thread, front to back, as `Write + Seek`: a
/// forward seek writes zeros, `End(0)` is the current position, going back is an error. The
/// first `skip` bytes are only counted (the server holds them already, from a write that was
/// cut off). [`Self::finish`] checks the file is exactly its `size`; dropped unfinished, the
/// write is given up and the partial file stays for the caller to remove.
pub(crate) struct Upload {
    tx: Option<SyncSender<Msg>>,
    thread: Option<std::thread::JoinHandle<Result<(), RemoteError>>>,
    /// Why the sending thread stopped, once a send found it gone.
    failed: Option<RemoteError>,
    buf: Vec<u8>,
    pos: u64,
    size: u64,
    skip: u64,
    path: String,
}

impl Upload {
    /// Opens `path` on `fs` for `size` bytes, written from byte `skip` on (0: a new or emptied
    /// file). Fails as the server does when the file cannot be opened.
    pub(crate) fn start(
        dest: &ServerDest,
        fs: Arc<dyn RemoteFs>,
        path: &str,
        size: u64,
        skip: u64,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, RemoteError> {
        let (tx, rx) = mpsc::sync_channel::<Msg>(PIPE);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), RemoteError>>();
        let handle = dest.handle.clone();
        let file = path.to_string();
        let thread = std::thread::Builder::new()
            .name("convert-to-server".into())
            .spawn(move || send(handle, fs, file, size, skip, cancel, rx, ready_tx))
            .map_err(|e| RemoteError::Io(format!("could not start the upload: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                return Err(match thread.join() {
                    Ok(Err(e)) => e,
                    _ => RemoteError::Io("the upload stopped before it began".into()),
                })
            }
        }
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            failed: None,
            buf: Vec::with_capacity(PIECE),
            pos: 0,
            size,
            skip: skip.min(size),
            path: path.to_string(),
        })
    }

    /// The sending thread's own failure, once it is gone.
    fn gone(&mut self) -> io::Error {
        self.tx = None;
        if self.failed.is_none() {
            self.failed = Some(match self.thread.take().map(|t| t.join()) {
                Some(Ok(Err(e))) => e,
                Some(Ok(Ok(()))) => RemoteError::Io("the upload ended early".into()),
                Some(Err(_)) => RemoteError::Io("the upload panicked".into()),
                None => RemoteError::Io("the upload stopped".into()),
            });
        }
        io::Error::other(
            self.failed
                .as_ref()
                .map_or_else(String::new, ToString::to_string),
        )
    }

    fn push(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let full = std::mem::replace(&mut self.buf, Vec::with_capacity(PIECE));
        let sent = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(Msg::Data(full)).is_ok());
        if sent {
            Ok(())
        } else {
            Err(self.gone())
        }
    }

    fn take(&mut self, mut data: &[u8]) -> io::Result<()> {
        if self
            .pos
            .checked_add(data.len() as u64)
            .is_none_or(|e| e > self.size)
        {
            return Err(io::Error::other(format!(
                "{} runs past its planned {} bytes",
                self.path, self.size
            )));
        }
        // What the server holds already is only counted.
        if self.pos < self.skip {
            let n = (self.skip - self.pos).min(data.len() as u64) as usize;
            self.pos += n as u64;
            data = &data[n..];
        }
        while !data.is_empty() {
            let n = (PIECE - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            self.pos += n as u64;
            data = &data[n..];
            if self.buf.len() == PIECE {
                self.push()?;
            }
        }
        Ok(())
    }

    fn zeros(&mut self, mut n: u64) -> io::Result<()> {
        static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
        while n > 0 {
            let k = n.min(ZEROS.len() as u64) as usize;
            self.take(&ZEROS[..k])?;
            n -= k as u64;
        }
        Ok(())
    }

    /// Bytes written so far, the skipped ones included.
    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    /// The last bytes, then the server's word that it holds the file: it must be exactly its
    /// planned size.
    pub(crate) fn finish(mut self) -> Result<(), RemoteError> {
        if self.pos != self.size {
            return Err(RemoteError::Io(format!(
                "{} is {} bytes, not the planned {}",
                self.path, self.pos, self.size
            )));
        }
        if let Err(e) = self.push() {
            return Err(self.failed.take().unwrap_or(RemoteError::Io(e.to_string())));
        }
        let ended = self.tx.take().is_some_and(|tx| tx.send(Msg::End).is_ok());
        let joined = self.thread.take().map(|t| t.join());
        match (ended, joined) {
            (_, Some(Ok(r))) => r,
            (_, Some(Err(_))) => Err(RemoteError::Io("the upload panicked".into())),
            (_, None) => Err(self
                .failed
                .take()
                .unwrap_or(RemoteError::Io("the upload stopped".into()))),
        }
    }
}

impl Drop for Upload {
    /// Unfinished: the sending thread gives the file up, and is waited for.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Write for Upload {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.take(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Upload {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(0) => Some(self.pos),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(_) => None,
        };
        match target {
            Some(t) if t >= self.pos => {
                self.zeros(t - self.pos)?;
                Ok(t)
            }
            _ => Err(io::Error::other(
                "a file written straight to a server is written forward only",
            )),
        }
    }
}

/// The sending thread: opens the file, then sends each piece as it comes, finishing at
/// [`Msg::End`] and giving the file up when the pipe closes without it.
#[allow(clippy::too_many_arguments)]
fn send(
    handle: tokio::runtime::Handle,
    fs: Arc<dyn RemoteFs>,
    path: String,
    size: u64,
    skip: u64,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Msg>,
    ready: mpsc::Sender<Result<(), RemoteError>>,
) -> Result<(), RemoteError> {
    let mut w = match handle.block_on(fs.create_writer(&path, size, skip)) {
        Ok(w) => {
            let _ = ready.send(Ok(()));
            w
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return Err(RemoteError::Io("not opened".into()));
        }
    };
    loop {
        match rx.recv() {
            Ok(Msg::Data(piece)) => {
                let wrote = handle.block_on(crate::remote::write_limited(
                    &mut *w,
                    &piece,
                    &cancel,
                    crate::remote::WRITE_STALL,
                    &path,
                ));
                if let Err(e) = wrote {
                    handle.block_on(w.abort());
                    return Err(e);
                }
            }
            Ok(Msg::End) => return handle.block_on(w.finish()),
            Err(_) => {
                handle.block_on(w.abort());
                return Err(RemoteError::Io("cancelled".into()));
            }
        }
    }
}

/// A folder build's folder on a server: `name` in the destination's folder.
pub(crate) struct ServerFolder<'a> {
    pub dest: &'a ServerDest,
    pub name: String,
    /// The job's cancel, which the sending threads listen to as well.
    pub cancel: Arc<AtomicBool>,
}

/// What a game needs to start: written last, under a temporary name until everything else is
/// in place and read back, so a folder cut off part way never launches.
fn is_launch_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p == "eboot.bin" || p == "sce_sys/param.json" || p == "sce_sys/param.sfo"
}

/// The temporary name a file is written under before it gets its own.
fn temp_name(path: &str) -> String {
    format!("{path}.ps5upload-tmp")
}

/// The folder read back with the launch files still under their temporary names, answering
/// to their own.
struct Aliased {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    /// Own name → the name on the server.
    to_server: std::collections::HashMap<String, String>,
}

impl Aliased {
    fn new(inner: Box<dyn SourceTree>, launch: &[String]) -> Self {
        let to_server: std::collections::HashMap<String, String> =
            launch.iter().map(|p| (p.clone(), temp_name(p))).collect();
        let from_server: std::collections::HashMap<&str, &str> = to_server
            .iter()
            .map(|(own, tmp)| (tmp.as_str(), own.as_str()))
            .collect();
        let mut files: Vec<SourceFile> = inner
            .files()
            .iter()
            .map(|f| SourceFile {
                path: from_server
                    .get(f.path.as_str())
                    .map_or_else(|| f.path.clone(), |own| (*own).to_string()),
                size: f.size,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            inner,
            files,
            to_server,
        }
    }

    fn on_server(&self, path: &str) -> String {
        self.to_server
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_string())
    }
}

impl SourceTree for Aliased {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        let p = self.on_server(path);
        self.inner.read(&p)
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        let p = self.on_server(path);
        self.inner.read_range(&p, offset, len)
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// What a folder build made on the server, removed again if it fails.
#[derive(Default)]
struct Made {
    files: Vec<String>,
    dirs: Vec<String>,
}

/// Writes `tree` as the folder `to` on the server: its paths checked, every file written under a
/// temporary name and then renamed to its own (never over anything), the launch files last and
/// left under their temporary names until the folder reads back (`check`), then renamed too.
/// Any failure or a cancel removes what was written.
pub(crate) fn write_folder(
    tree: &mut dyn SourceTree,
    to: &ServerFolder,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    check: &mut crate::folder_build::Check,
) -> Result<crate::folder_build::FolderBuilt, String> {
    if tree.files().is_empty() {
        return Err(format!("{} has no files in it", tree.describe()));
    }
    let dest = to.dest;
    let fs = dest.fs().map_err(|e| said(&e))?;
    let root = dest.path_of(&to.name);
    dest.make_dir(&*fs)
        .map_err(|e| format!("making {} on {}: {}", dest.dir, dest.label, said(&e)))?;
    if dest.exists(&*fs, &root).map_err(|e| said(&e))? {
        return Err(format!(
            "{root} already exists on {}: delete it there or give the folder another name",
            dest.label
        ));
    }
    crate::folder_build::check_server_paths(tree, dest.windows_names)?;
    let listed: Vec<(String, u64)> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let empty_dirs: Vec<String> = tree.empty_dirs().to_vec();
    let total: u64 = listed.iter().map(|f| f.1).sum();
    dest.room_check(&*fs, total)?;
    let mut made = Made::default();
    let result = (|| -> Result<crate::folder_build::FolderBuilt, String> {
        dest.block_on(fs.create_dir(&root))
            .map_err(|e| format!("making {root}: {}", said(&e)))?;
        made.dirs.push(root.clone());
        let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut make = |rel: &str, made: &mut Made| -> Result<(), String> {
            let mut at = String::new();
            for part in rel.split('/').filter(|p| !p.is_empty()) {
                at = if at.is_empty() {
                    part.to_string()
                } else {
                    format!("{at}/{part}")
                };
                if have.insert(at.clone()) {
                    let full = format!("{root}/{at}");
                    dest.block_on(fs.create_dir(&full))
                        .map_err(|e| format!("making {full}: {}", said(&e)))?;
                    made.dirs.push(full);
                }
            }
            Ok(())
        };
        stage("write", 0, total);
        for d in &empty_dirs {
            make(d.trim_matches('/'), &mut made)?;
        }
        // Everything else first, the launch files last.
        let mut order: Vec<&(String, u64)> = listed.iter().collect();
        order.sort_by_key(|(p, _)| is_launch_file(p));
        let mut written = 0u64;
        let mut launch: Vec<String> = Vec::new();
        for (path, size) in order {
            if let Some((parent, _)) = path.rsplit_once('/') {
                make(parent, &mut made)?;
            }
            let full = format!("{root}/{path}");
            let tmp = temp_name(&full);
            made.files.push(tmp.clone());
            copy_file(
                tree,
                path,
                *size,
                dest,
                &fs,
                &tmp,
                &to.cancel,
                cancel,
                &mut |n| {
                    written += n;
                    stage("write", written, total);
                },
            )?;
            if is_launch_file(path) {
                launch.push(path.clone());
                continue;
            }
            rename(dest, &*fs, &tmp, &full)?;
            made.files.pop();
            made.files.push(full);
        }
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let files = dest.files().map_err(|e| said(&e))?;
        let opened = ps5upload_fpkg::remote_source::open_remote(files, &root)
            .map_err(|e| format!("the folder could not be read back on {}: {e}", dest.label))?;
        let view = Aliased::new(opened, &launch);
        let checked = check(
            crate::folder_build::Written::Tree(Box::new(view)),
            &listed,
            &empty_dirs,
            stage,
        )?;
        // Read back whole: now it may start.
        for path in &launch {
            let full = format!("{root}/{path}");
            let tmp = temp_name(&full);
            rename(dest, &*fs, &tmp, &full)?;
            if let Some(i) = made.files.iter().position(|f| *f == tmp) {
                made.files[i] = full;
            }
        }
        Ok(crate::folder_build::FolderBuilt {
            files: listed.len() as u64,
            bytes: total,
            detail: format!(
                "{} files, {} empty folders, {total} bytes, written to {}, {checked}",
                listed.len(),
                empty_dirs.len(),
                dest.label,
            ),
        })
    })();
    if result.is_err() {
        remove_made(dest, &*fs, made);
    }
    match result {
        Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => Err("cancelled".into()),
        r => r,
    }
}

/// Renames `from` to `to` on the server, never over anything there.
fn rename(dest: &ServerDest, fs: &dyn RemoteFs, from: &str, to: &str) -> Result<(), String> {
    dest.block_on(fs.rename(from, to)).map_err(|e| match e {
        RemoteError::Exists(_) => format!("{to} appeared on {} while it was written", dest.label),
        e => format!("renaming {from} to {to}: {}", said(&e)),
    })
}

/// Removes what a failed folder build made: its files, then its folders, deepest first. A
/// server that cannot remove a folder (WebDAV removes one only with everything in it) keeps
/// the empty ones.
fn remove_made(dest: &ServerDest, fs: &dyn RemoteFs, made: Made) {
    for f in &made.files {
        let _ = dest.block_on(fs.remove_file(f));
    }
    let mut dirs = made.dirs;
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    let mut kept = 0;
    for d in &dirs {
        if dest.block_on(fs.remove_dir(d)).is_err() {
            kept += 1;
        }
    }
    if kept > 0 {
        crate::log_at!(
            Info,
            "convert",
            "to server: {kept} empty folder(s) left under {} on {}",
            dest.dir,
            dest.label
        );
    }
}

/// Copies `path` (`size` bytes) out of `tree` to `to` on the server.
#[allow(clippy::too_many_arguments)]
fn copy_file(
    tree: &mut dyn SourceTree,
    path: &str,
    size: u64,
    dest: &ServerDest,
    fs: &Arc<dyn RemoteFs>,
    to: &str,
    job_cancel: &Arc<AtomicBool>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let mut up = Upload::start(dest, Arc::clone(fs), to, size, 0, Arc::clone(job_cancel))
        .map_err(|e| format!("{path}: {}", said(&e)))?;
    let mut at = 0u64;
    while at < size {
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let want = crate::folder_build::CHUNK.min(size - at) as usize;
        let bytes = tree
            .read_range(path, at, want)
            .map_err(|e| format!("{path}: {e}"))?;
        if bytes.is_empty() {
            return Err(format!("{path}: the source ended at byte {at} of {size}"));
        }
        let bytes = &bytes[..bytes.len().min(want)];
        up.write_all(bytes).map_err(|e| format!("{path}: {e}"))?;
        at += bytes.len() as u64;
        progress(bytes.len() as u64);
    }
    up.finish().map_err(|e| format!("{path}: {}", said(&e)))
}

#[cfg(test)]
mod tests;
