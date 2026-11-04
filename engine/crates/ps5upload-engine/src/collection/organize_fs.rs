//! Where the organizer's files are: this computer's disk, or a folder on a saved server.
//!
//! The organizer only ever lists, looks up, makes folders, renames without replacing, and
//! removes folders it emptied. [`Tree`] is that, for both. Paths are `PathBuf`s holding what
//! the Collection holds: a local path, or a `remote://<connection>/<path>` string (the server
//! form is joined and compared like any path, and parsed back only at the server's door).
//!
//! A server tree blocks on the runtime, so it is used from `spawn_blocking`, never from a
//! runtime worker. Its listings are kept for the life of the tree (one request) and dropped on
//! every write, so a plan costs one round trip per folder and an apply's checks see the server
//! as it is now.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::remote::pool::{is_transient, Pool};
use crate::remote::store::Store;
use crate::remote::{RemoteError, RemoteFs};

/// One entry of a folder listing.
#[derive(Debug, Clone)]
pub struct DirEnt {
    pub name: String,
    pub is_dir: bool,
    /// A regular file (a symlink on this computer is neither file nor folder).
    pub is_file: bool,
    pub size: u64,
    /// Seconds since the epoch; 0 when unknown.
    pub mtime: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpError {
    /// The name is taken; nothing was changed.
    Exists(String),
    /// The server stopped answering and a fresh sign-in did not help. What the last operation
    /// did is unknown until the server can be asked again.
    Lost(String),
    Other(String),
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Exists(m) | OpError::Lost(m) | OpError::Other(m) => f.write_str(m),
        }
    }
}

pub trait Tree: Send + Sync {
    /// The entries of `dir`, or `None` when it cannot be read.
    fn list(&self, dir: &Path) -> Option<Vec<DirEnt>>;
    /// A file's size and time; may come from the listing of its folder.
    fn meta(&self, p: &Path) -> Option<Meta>;
    /// Whether anything is at `p` now (never from a cached listing).
    fn exists(&self, p: &Path) -> Result<bool, OpError>;
    /// One folder; one already there is success.
    fn create_dir(&self, p: &Path) -> Result<(), OpError>;
    /// Renames without ever replacing what is at `to`.
    fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError>;
    /// Removes an empty folder.
    fn remove_dir(&self, p: &Path) -> Result<(), OpError>;
    /// A file of Finder's that may be deleted to empty a folder. Never on a server: the
    /// organizer deletes no file there.
    fn is_removable_junk(&self, p: &Path) -> bool;
    fn remove_junk(&self, p: &Path);
    /// Something that went wrong reading the tree (a folder that could not be listed), for
    /// the preview to say so rather than show a quietly smaller plan.
    fn trouble(&self) -> Option<String> {
        None
    }
    /// A file's first four bytes (does it start a package?), or `None` when they cannot be read.
    fn head(&self, _p: &Path) -> Option<[u8; 4]> {
        None
    }
}

/// One plan's view of a tree: each folder is listed once and each file's first bytes are read
/// once, however many packages ask. (A plan asks about a package's folder several times per
/// package, which on a big folder is a walk of it per package.) Writes go straight through.
pub struct PlanTree<'a> {
    inner: &'a dyn Tree,
    lists: Mutex<HashMap<std::path::PathBuf, Option<Vec<DirEnt>>>>,
    heads: Mutex<HashMap<std::path::PathBuf, Option<[u8; 4]>>>,
}

impl<'a> PlanTree<'a> {
    pub fn new(inner: &'a dyn Tree) -> Self {
        Self {
            inner,
            lists: Mutex::new(HashMap::new()),
            heads: Mutex::new(HashMap::new()),
        }
    }
}

impl Tree for PlanTree<'_> {
    fn list(&self, dir: &Path) -> Option<Vec<DirEnt>> {
        if let Some(v) = self
            .lists
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return v.clone();
        }
        let v = self.inner.list(dir);
        self.lists
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_path_buf(), v.clone());
        v
    }

    fn meta(&self, p: &Path) -> Option<Meta> {
        let from_listing = p.parent().zip(p.file_name()).and_then(|(dir, name)| {
            self.list(dir)?
                .into_iter()
                .find(|e| name.to_string_lossy() == e.name.as_str())
                .map(|e| Meta {
                    size: e.size,
                    mtime: e.mtime,
                })
        });
        from_listing.or_else(|| self.inner.meta(p))
    }

    fn exists(&self, p: &Path) -> Result<bool, OpError> {
        self.inner.exists(p)
    }

    fn create_dir(&self, p: &Path) -> Result<(), OpError> {
        self.inner.create_dir(p)
    }

    fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError> {
        self.inner.rename_new(from, to)
    }

    fn remove_dir(&self, p: &Path) -> Result<(), OpError> {
        self.inner.remove_dir(p)
    }

    fn is_removable_junk(&self, p: &Path) -> bool {
        self.inner.is_removable_junk(p)
    }

    fn remove_junk(&self, p: &Path) {
        self.inner.remove_junk(p);
    }

    fn trouble(&self) -> Option<String> {
        self.inner.trouble()
    }

    fn head(&self, p: &Path) -> Option<[u8; 4]> {
        if let Some(v) = self.heads.lock().unwrap_or_else(|e| e.into_inner()).get(p) {
            return *v;
        }
        let v = self.inner.head(p);
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.to_path_buf(), v);
        v
    }
}

// ─── This computer ─────────────────────────────────────────────────────

pub struct LocalTree;

fn secs(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn io(e: std::io::Error) -> OpError {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        OpError::Exists(e.to_string())
    } else {
        OpError::Other(e.to_string())
    }
}

impl Tree for LocalTree {
    fn list(&self, dir: &Path) -> Option<Vec<DirEnt>> {
        let rd = std::fs::read_dir(dir).ok()?;
        Some(
            rd.flatten()
                .map(|e| {
                    let ft = e.file_type().ok();
                    let m = e.metadata().ok();
                    DirEnt {
                        name: e.file_name().to_string_lossy().into_owned(),
                        is_dir: ft.is_some_and(|t| t.is_dir()),
                        is_file: ft.is_some_and(|t| t.is_file()),
                        size: m.as_ref().map(|m| m.len()).unwrap_or(0),
                        mtime: m.map(|m| secs(m.modified())).unwrap_or(0),
                    }
                })
                .collect(),
        )
    }

    fn meta(&self, p: &Path) -> Option<Meta> {
        let m = std::fs::metadata(p).ok()?;
        Some(Meta {
            size: m.len(),
            mtime: secs(m.modified()),
        })
    }

    fn exists(&self, p: &Path) -> Result<bool, OpError> {
        match std::fs::symlink_metadata(p) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io(e)),
        }
    }

    fn create_dir(&self, p: &Path) -> Result<(), OpError> {
        match std::fs::create_dir(p) {
            Ok(()) => Ok(()),
            Err(_) if p.is_dir() => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError> {
        if self.exists(to)? {
            return Err(OpError::Exists(format!("{} already exists", to.display())));
        }
        // A rename, never a copy: a move across volumes fails instead of copying.
        std::fs::rename(from, to).map_err(io)
    }

    fn remove_dir(&self, p: &Path) -> Result<(), OpError> {
        std::fs::remove_dir(p).map_err(io)
    }

    fn is_removable_junk(&self, p: &Path) -> bool {
        super::junk::is_removable_junk(p)
    }

    fn remove_junk(&self, p: &Path) {
        if super::junk::is_removable_junk(p) {
            let _ = std::fs::remove_file(p);
        }
    }

    fn head(&self, p: &Path) -> Option<[u8; 4]> {
        let mut head = [0u8; 4];
        std::io::Read::read_exact(&mut std::fs::File::open(p).ok()?, &mut head).ok()?;
        Some(head)
    }
}

// ─── A saved server ────────────────────────────────────────────────────

pub struct RemoteTree {
    handle: tokio::runtime::Handle,
    pool: Arc<Pool>,
    store: Arc<Store>,
    id: String,
    /// Server path → its listing, until the next write.
    cache: Mutex<HashMap<String, Vec<DirEnt>>>,
    trouble: Mutex<Option<String>>,
}

impl RemoteTree {
    /// A tree on connection `id`. Call from a blocking thread inside the runtime.
    pub fn new(
        handle: tokio::runtime::Handle,
        pool: Arc<Pool>,
        store: Arc<Store>,
        id: &str,
    ) -> Self {
        Self {
            handle,
            pool,
            store,
            id: id.to_string(),
            cache: Mutex::new(HashMap::new()),
            trouble: Mutex::new(None),
        }
    }

    /// The tree for the server a `remote://` path is on, through the engine's pool.
    pub fn for_path(path: &str) -> Result<Self, String> {
        let (id, _) = split_raw(path).ok_or_else(|| format!("not a server path: {path}"))?;
        let r = crate::remote::pool::global().map_err(|e| e.to_string())?;
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| format!("{path}: no runtime to reach the server from"))?;
        Ok(Self::new(
            handle,
            Arc::clone(&r.pool),
            Arc::clone(&r.store),
            &id,
        ))
    }

    /// The server path in a `remote://` path on this tree's connection.
    fn server(&self, p: &Path) -> Result<String, OpError> {
        let s = path_string(p);
        let (id, path) =
            split_raw(&s).ok_or_else(|| OpError::Other(format!("not a server path: {s}")))?;
        if id != self.id {
            return Err(OpError::Other(format!("{s} is on another server")));
        }
        Ok(path)
    }

    fn forget(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// `f` on the session, once more on a fresh sign-in if the connection dropped. Only for
    /// what is safe to repeat.
    fn run<T, F, Fut>(&self, f: F) -> Result<T, RemoteError>
    where
        F: Fn(Arc<dyn RemoteFs>) -> Fut,
        Fut: std::future::Future<Output = Result<T, RemoteError>>,
    {
        self.handle
            .block_on(self.pool.with_fs(&self.store, &self.id, f))
    }

    fn list_server(&self, dir: &str) -> Result<Vec<DirEnt>, RemoteError> {
        if let Some(v) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return Ok(v.clone());
        }
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.run(|fs| {
                let (dir, cursor) = (dir.to_string(), cursor.clone());
                async move { fs.list(&dir, cursor).await }
            })?;
            out.extend(page.entries.into_iter().map(|e| DirEnt {
                is_file: !e.is_dir,
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
                mtime: e.mtime.unwrap_or(0),
            }));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        if cfg!(windows) {
            // A path here is a `PathBuf`, whose separator on Windows is also `\`: a name with
            // one in it could not be told from two names, so it is left out (never touched).
            out.retain(|e| !e.name.contains('\\'));
        }
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_string(), out.clone());
        Ok(out)
    }

    fn exists_server(&self, p: &str) -> Result<bool, OpError> {
        match self.run(|fs| {
            let p = p.to_string();
            async move { fs.stat(&p).await }
        }) {
            Ok(_) => Ok(true),
            Err(RemoteError::NotFound(_)) => Ok(false),
            Err(e) => Err(op(e)),
        }
    }

    /// A write's error, told apart: an error that looks like a dropped connection is
    /// [`OpError::Lost`] only when the server does not answer a look at its root either (a
    /// server can fail one request, an HTTP 500 say, and be fine).
    fn classify(&self, e: RemoteError) -> OpError {
        match op(e) {
            OpError::Lost(m) if self.exists_server("/").is_ok() => OpError::Other(m),
            other => other,
        }
    }

    fn rename_once(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        self.handle.block_on(async {
            let fs = self.pool.fs(&self.store, &self.id).await?;
            fs.rename(from, to).await
        })
    }
}

fn op(e: RemoteError) -> OpError {
    match e {
        RemoteError::Exists(m) => OpError::Exists(m),
        e if is_transient(&e) => OpError::Lost(e.to_string()),
        e => OpError::Other(e.to_string()),
    }
}

impl Tree for RemoteTree {
    fn list(&self, dir: &Path) -> Option<Vec<DirEnt>> {
        let d = self.server(dir).ok()?;
        match self.list_server(&d) {
            Ok(v) => Some(v),
            Err(RemoteError::NotFound(_)) => None,
            Err(e) => {
                let mut t = self.trouble.lock().unwrap_or_else(|e| e.into_inner());
                if t.is_none() {
                    *t = Some(format!("{d} could not be listed: {e}"));
                }
                None
            }
        }
    }

    fn meta(&self, p: &Path) -> Option<Meta> {
        let s = self.server(p).ok()?;
        let (dir, name) = s.rsplit_once('/')?;
        let dir = if dir.is_empty() { "/" } else { dir };
        if let Ok(list) = self.list_server(dir) {
            return list.iter().find(|e| e.name == name).map(|e| Meta {
                size: e.size,
                mtime: e.mtime,
            });
        }
        let e = self
            .run(|fs| {
                let s = s.clone();
                async move { fs.stat(&s).await }
            })
            .ok()?;
        Some(Meta {
            size: e.size,
            mtime: e.mtime.unwrap_or(0),
        })
    }

    fn exists(&self, p: &Path) -> Result<bool, OpError> {
        self.exists_server(&self.server(p)?)
    }

    fn create_dir(&self, p: &Path) -> Result<(), OpError> {
        let s = self.server(p)?;
        self.forget();
        // Safe to repeat: a folder already there is success.
        self.run(|fs| {
            let s = s.clone();
            async move { fs.create_dir(&s).await }
        })
        .map_err(|e| self.classify(e))
    }

    /// A rename is not safe to repeat blindly: the first try may have happened with its reply
    /// lost. After a dropped connection the server is asked (on a fresh sign-in) where the
    /// file is: at the new name only means done; at the old name only, it is tried once more;
    /// anything else, or no answer, is [`OpError::Lost`].
    fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError> {
        let (f, t) = (self.server(from)?, self.server(to)?);
        self.forget();
        let first = self.rename_once(&f, &t);
        let e = match first {
            Ok(()) => return Ok(()),
            Err(e) if !is_transient(&e) => return Err(op(e)),
            Err(e) => e,
        };
        self.pool.invalidate(&self.id);
        let at_old = self.exists_server(&f);
        let at_new = self.exists_server(&t);
        self.forget();
        match (at_old, at_new) {
            (Ok(false), Ok(true)) => Ok(()),
            (Ok(true), Ok(false)) => self.rename_once(&f, &t).map_err(|e| self.classify(e)),
            (Err(OpError::Lost(_)), _) | (_, Err(OpError::Lost(_))) => {
                Err(OpError::Lost(e.to_string()))
            }
            _ => Err(OpError::Other(e.to_string())),
        }
    }

    fn remove_dir(&self, p: &Path) -> Result<(), OpError> {
        let s = self.server(p)?;
        self.forget();
        match self.run(|fs| {
            let s = s.clone();
            async move { fs.remove_dir(&s).await }
        }) {
            Ok(()) => Ok(()),
            // Gone either way (a repeat after a lost reply lands here).
            Err(RemoteError::NotFound(_)) if self.exists_server(&s) == Ok(false) => Ok(()),
            Err(e) => Err(self.classify(e)),
        }
    }

    fn is_removable_junk(&self, _p: &Path) -> bool {
        false
    }

    fn remove_junk(&self, _p: &Path) {}

    fn trouble(&self) -> Option<String> {
        self.trouble
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn head(&self, p: &Path) -> Option<[u8; 4]> {
        let s = self.server(p).ok()?;
        let bytes = self
            .run(|fs| {
                let s = s.clone();
                async move { fs.open(&s).await?.read_at(0, 4).await }
            })
            .ok()?;
        bytes.get(..4)?.try_into().ok()
    }
}

/// A path as the organizer shows and stores it. On Windows a server path joined as a `PathBuf`
/// has `\` separators, which go back to `/`; anywhere else it is left as it is (on a server a
/// `\` is a name character).
pub fn path_string(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) && crate::remote::path::is_remote(&s) {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// `remote://<connection><server path>` in the organizer's form: the server path with its
/// names as the server gives them (never percent-decoded, so "a%20b.pkg" is that name).
/// Returns (connection id, server path), or `None` for anything else or a `.`/`..` segment.
pub fn split_raw(s: &str) -> Option<(String, String)> {
    let rest = s.strip_prefix("remote://")?;
    let (id, path) = rest.split_once('/').unwrap_or((rest, ""));
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let mut segs = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" => continue,
            "." | ".." => return None,
            s if s.contains('\0') => return None,
            s => segs.push(s),
        }
    }
    Some((id.to_string(), format!("/{}", segs.join("/"))))
}

/// A collection root (as the settings hold it) in the organizer's form: a server root's path
/// decoded once into plain names.
pub fn raw_root(root: &str) -> std::path::PathBuf {
    if crate::remote::path::is_remote(root) {
        if let Ok(p) = crate::remote::path::parse(root) {
            return std::path::PathBuf::from(format!("remote://{}{}", p.connection_id, p.path));
        }
    }
    std::path::PathBuf::from(root)
}

/// The tree a collection path is in: a server's for a `remote://` path, else this computer.
pub fn tree_for(path: &Path) -> Result<Box<dyn Tree>, String> {
    let s = path_string(path);
    if crate::remote::path::is_remote(&s) {
        Ok(Box::new(RemoteTree::for_path(&s)?))
    } else {
        Ok(Box::new(LocalTree))
    }
}

#[cfg(test)]
mod tests {
    use super::super::organize::{self, Facts};
    use super::super::PkgInfo;
    use super::*;
    use crate::remote::pool::testing::remote_with_shared;
    use crate::remote::store::{conn, Protocol, Secret};
    use crate::remote::MemFs;
    use std::path::PathBuf;

    fn info(title: &str, tid: &str) -> PkgInfo {
        PkgInfo {
            platform: "PS4".into(),
            title: title.into(),
            title_id: tid.into(),
            content_id: format!("UP0000-{tid}_00-AAAAAAAAAAAAAAAA"),
            version: "1.00".into(),
            region: "Americas".into(),
            kind: "base".into(),
            kind_confident: true,
            complete: true,
            ..PkgInfo::default()
        }
    }

    /// Facts by file-name prefix, read through the tree (sizes from the server's listing).
    fn facts(tree: &dyn Tree, p: &Path) -> Facts {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let parts = organize::split_siblings(tree, p);
        let size = parts
            .iter()
            .filter_map(|q| tree.meta(q))
            .map(|m| m.size)
            .sum();
        let i = match () {
            _ if name.starts_with("rdr") || name.starts_with("RDR2") => info("RDR2", "CUSA03041"),
            _ if name.starts_with("bb") || name.starts_with("Bloodborne") => {
                info("Bloodborne", "CUSA00900")
            }
            _ if name.starts_with("jak") || name.starts_with("Jak") => info("Jak X", "CUSA07842"),
            _ => PkgInfo::default(),
        };
        Facts {
            path: p.to_path_buf(),
            parts,
            size,
            info: i,
        }
    }

    struct Server {
        mem: Arc<MemFs>,
        pool: Arc<Pool>,
        store: Arc<Store>,
        id: String,
        logs: PathBuf,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.logs);
        }
    }

    fn server(name: &str, files: &[(&str, &[u8])]) -> Server {
        let mem = Arc::new(MemFs::new(files));
        let r = remote_with_shared(Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let logs = std::env::temp_dir().join(format!("ps5u-rorg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&logs);
        Server {
            mem,
            pool: Arc::clone(&r.pool),
            store: Arc::clone(&r.store),
            id,
            logs,
        }
    }

    impl Server {
        fn tree(&self) -> RemoteTree {
            RemoteTree::new(
                tokio::runtime::Handle::current(),
                Arc::clone(&self.pool),
                Arc::clone(&self.store),
                &self.id,
            )
        }
        fn path(&self, p: &str) -> PathBuf {
            PathBuf::from(format!("remote://{}{p}", self.id))
        }
    }

    /// Runs `f` where a server tree may block: off the runtime's workers.
    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(f).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_folder_is_planned_applied_and_undone() {
        let s = Arc::new(server(
            "cycle",
            &[
                ("/games/fpkg/rdr.pkg", b"rdr2-bytes"),
                ("/games/fpkg/old/bb.pkg", b"bloodborne"),
                ("/games/fpkg/notes.txt", b"keep"),
            ],
        ));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/games/fpkg");
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            assert_eq!(plan.moves.len(), 2, "{:?}", plan.moves);
            assert!(plan
                .moves
                .iter()
                .all(|m| m.container == c.to_string_lossy() && m.to.starts_with("PS4/")));
            let log_path = s.logs.join("run.json");
            let log =
                organize::apply(&tree, &c, plan.moves.clone(), &log_path, "NAS", None).unwrap();
            assert_eq!(log.completed.len(), 2, "{:?}", log.failed);
            assert_eq!(log.server, "NAS");
            for m in &plan.moves {
                let to = format!("/games/fpkg/{}", m.to);
                assert!(s.mem.bytes(&to).is_some(), "{to}");
                assert!(s.mem.bytes(&format!("/games/fpkg/{}", m.from)).is_none());
            }
            // The folder the run emptied is gone; other files stay.
            assert!(log.removed_dirs.contains(&"old".to_string()), "{log:?}");
            assert_eq!(s.mem.bytes("/games/fpkg/notes.txt").unwrap(), b"keep");

            let again = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            assert!(again.moves.is_empty(), "{:?}", again.moves);

            let id = s.id.clone();
            let (pool, store) = (Arc::clone(&s.pool), Arc::clone(&s.store));
            let open = move |_: &Path| -> Result<Box<dyn Tree>, String> {
                Ok(Box::new(RemoteTree::new(
                    tokio::runtime::Handle::current(),
                    Arc::clone(&pool),
                    Arc::clone(&store),
                    &id,
                )))
            };
            let undone = organize::revert(&log_path, &open).unwrap();
            assert_eq!(undone.reverted.len(), 2, "{:?}", undone.problems);
            assert_eq!(s.mem.bytes("/games/fpkg/rdr.pkg").unwrap(), b"rdr2-bytes");
            assert_eq!(
                s.mem.bytes("/games/fpkg/old/bb.pkg").unwrap(),
                b"bloodborne"
            );
            // The folders the run made are gone again.
            assert!(s.tree().list(&s.path("/games/fpkg/PS4")).is_none());
            assert!(organize::revert(&log_path, &open).is_err(), "undone once");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_taken_on_the_server_since_the_preview_is_never_overwritten() {
        let s = Arc::new(server("taken", &[("/g/rdr.pkg", b"mine")]));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/g");
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            let to = format!("/g/{}", plan.moves[0].to);
            // Someone else's file lands at the destination after the preview.
            s.mem.add(&[(to.as_str(), b"theirs")]);
            let log =
                organize::apply(&tree, &c, plan.moves, &s.logs.join("l.json"), "", None).unwrap();
            assert_eq!(log.completed.len(), 0);
            assert_eq!(log.failed[0].error, "destination appeared");
            assert_eq!(s.mem.bytes(&to).unwrap(), b"theirs");
            assert_eq!(s.mem.bytes("/g/rdr.pkg").unwrap(), b"mine");
            // And the tree's own rename refuses a taken name, leaving both.
            let r = tree.rename_new(
                &s.path("/g/rdr.pkg"),
                &PathBuf::from(format!("remote://{}{to}", s.id)),
            );
            assert!(matches!(r, Err(OpError::Exists(_))), "{r:?}");
            assert_eq!(s.mem.bytes(&to).unwrap(), b"theirs");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rename_whose_reply_was_lost_is_not_repeated() {
        let s = Arc::new(server("lost-reply", &[("/g/a.pkg", b"a")]));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            s.mem.lose_next_replies(1);
            tree.rename_new(&s.path("/g/a.pkg"), &s.path("/g/b.pkg"))
                .expect("done, though its reply was lost");
            assert_eq!(s.mem.bytes("/g/b.pkg").unwrap(), b"a");
            assert!(s.mem.bytes("/g/a.pkg").is_none());
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_lost_mid_run_stops_it_and_undo_puts_back_what_happened() {
        let s = Arc::new(server(
            "down",
            &[
                ("/g/bb.pkg", b"bloodborne"),
                ("/g/jak.pkg", b"jak"),
                ("/g/rdr.pkg", b"rdr2"),
            ],
        ));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/g");
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            assert_eq!(plan.moves.len(), 3);
            // Move 1: make PS4, make its title folder, rename (3 writes). Move 2: make its
            // title folder, then the rename happens but the server drops with its reply.
            s.mem.go_down_after_writes(5);
            let log_path = s.logs.join("down.json");
            let log = organize::apply(&tree, &c, plan.moves.clone(), &log_path, "", None).unwrap();
            assert_eq!(log.completed.len(), 1, "{log:?}");
            assert_eq!(log.failed.len(), 2);
            assert!(log.failed[0].uncertain, "{:?}", log.failed[0]);
            assert!(!log.failed[1].uncertain);
            assert!(log.failed[1].error.starts_with("not tried"));
            // What the log on disk says is what happened.
            let saved: organize::UndoLog =
                serde_json::from_slice(&std::fs::read(&log_path).unwrap()).unwrap();
            assert_eq!(saved.completed.len(), 1);

            // Undo while the server is still away: refused, nothing judged gone.
            let open_tree = {
                let (pool, store, id) = (Arc::clone(&s.pool), Arc::clone(&s.store), s.id.clone());
                move |_: &Path| -> Result<Box<dyn Tree>, String> {
                    Ok(Box::new(RemoteTree::new(
                        tokio::runtime::Handle::current(),
                        Arc::clone(&pool),
                        Arc::clone(&store),
                        &id,
                    )))
                }
            };
            assert!(organize::revert(&log_path, &open_tree).is_err());

            // Back up: the completed move and the uncertain one (it did happen) go back.
            s.mem.set_down(false);
            let undone = organize::revert(&log_path, &open_tree).unwrap();
            assert_eq!(undone.reverted.len(), 2, "{:?}", undone.problems);
            for (p, b) in [
                ("/g/bb.pkg", &b"bloodborne"[..]),
                ("/g/jak.pkg", b"jak"),
                ("/g/rdr.pkg", b"rdr2"),
            ] {
                assert_eq!(s.mem.bytes(p).as_deref(), Some(b), "{p}");
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_index_spares_reading_packages_over_the_network() {
        let s = Arc::new(server(
            "cached",
            &[("/games/fpkg/x.pkg", b"not a real package")],
        ));
        s.mem.set_mtime("/games/fpkg/x.pkg", 1_700_000_000);
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let root = s.path("/games");
            let mut lib = super::super::Library::default();
            lib.pkg_cache.insert(
                format!("{}|fpkg/x.pkg", root.display()),
                super::super::PkgCacheEntry {
                    size: 18,
                    mtime: 1_700_000_000,
                    pkg: info("RDR2", "CUSA03041"),
                    v: super::super::IDENTIFY_VERSION,
                },
            );
            let c = root.join("fpkg");
            let roots = [(root.clone(), root.display().to_string())];
            let plan = organize::plan_cached(&tree, &c, Some(&lib), &roots);
            assert_eq!(plan.moves.len(), 1, "{:?}", plan.skipped);
            assert!(plan.moves[0].to.starts_with("PS4/CUSA03041 - RDR2/"));
            // A changed file is read again rather than trusted.
            s.mem.set_mtime("/games/fpkg/x.pkg", 1_700_000_001);
            let tree = s.tree();
            let plan = organize::plan_cached(&tree, &c, Some(&lib), &roots);
            assert!(plan.moves.is_empty());
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_never_removes_folders_keeps_the_emptied_ones_and_says_so() {
        let s = Arc::new(server("keep", &[("/g/old/rdr.pkg", b"rdr2")]));
        s.mem.never_remove_dirs();
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/g");
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            let log =
                organize::apply(&tree, &c, plan.moves, &s.logs.join("k.json"), "", None).unwrap();
            assert_eq!(log.completed.len(), 1);
            assert!(log.removed_dirs.is_empty());
            assert_eq!(log.kept_dirs, vec!["old".to_string()]);
            assert!(s
                .tree()
                .list(&s.path("/g/old"))
                .is_some_and(|v| v.is_empty()));
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn odd_server_names_are_renamed_as_themselves() {
        let s = Arc::new(server(
            "names",
            &[
                ("/g/rdr%20x.pkg", b"percent"),
                ("/g/rdr x.pkg", b"space"),
                ("/g/bb 100% .pkg", b"orange"),
                ("/g/jak\\b.pkg", b"backslash"),
            ],
        ));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/g");
            for (name, bytes) in [
                ("rdr%20x.pkg", &b"percent"[..]),
                ("rdr x.pkg", b"space"),
                ("bb 100% .pkg", b"orange"),
                ("jak\\b.pkg", b"backslash"),
            ] {
                let m = tree.meta(&c.join(name)).expect(name);
                assert_eq!(m.size, bytes.len() as u64, "{name}");
            }
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            let froms: Vec<&str> = plan.moves.iter().map(|m| m.from.as_str()).collect();
            for name in ["rdr%20x.pkg", "rdr x.pkg", "bb 100% .pkg", "jak\\b.pkg"] {
                assert!(
                    froms.contains(&name),
                    "{name} not planned as itself: {froms:?}"
                );
            }
            let log = organize::apply(
                &tree,
                &c,
                plan.moves.clone(),
                &s.logs.join("n.json"),
                "",
                None,
            )
            .unwrap();
            assert_eq!(log.completed.len(), 4, "{:?}", log.failed);
            for m in &plan.moves {
                let want: &[u8] = match m.from.as_str() {
                    "rdr%20x.pkg" => b"percent",
                    "rdr x.pkg" => b"space",
                    "bb 100% .pkg" => b"orange",
                    _ => b"backslash",
                };
                assert_eq!(
                    s.mem.bytes(&format!("/g/{}", m.to)).as_deref(),
                    Some(want),
                    "{}",
                    m.from
                );
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_taken_in_another_case_is_not_landed_on() {
        let s = Arc::new(server("case", &[("/g/rdr.pkg", b"mine")]));
        let s2 = Arc::clone(&s);
        blocking(move || {
            let s = s2;
            let tree = s.tree();
            let c = s.path("/g");
            let first = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            let to = first.moves[0].to.clone();
            // The same name in another case, already there (a case-insensitive server's twin).
            let shout = format!("/g/{}", to.to_uppercase());
            s.mem.add(&[(shout.as_str(), b"theirs")]);
            let tree = s.tree();
            let plan = organize::plan_with(&tree, &c, &|p| facts(&tree, p));
            assert_eq!(plan.moves.len(), 1);
            assert_ne!(
                plan.moves[0].to.to_lowercase(),
                to.to_lowercase(),
                "{plan:?}"
            );
            let log = organize::apply(
                &tree,
                &c,
                plan.moves.clone(),
                &s.logs.join("c.json"),
                "",
                None,
            )
            .unwrap();
            assert_eq!(log.completed.len(), 1);
            assert_eq!(s.mem.bytes(&shout).unwrap(), b"theirs");
            assert_eq!(
                s.mem.bytes(&format!("/g/{}", plan.moves[0].to)).unwrap(),
                b"mine"
            );
        })
        .await;
    }
}
