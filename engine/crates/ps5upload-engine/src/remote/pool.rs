//! Signed-in sessions, one per saved connection, reused while browsing and streaming.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::store::{Connection, Protocol, Secret, Store};
use super::{RemoteError, RemoteFile, RemoteFs};

/// A session unused this long is closed; the next use signs in again.
const IDLE: Duration = Duration::from_secs(120);

/// The store and pool the engine serves from, created on first use in the data folder.
pub struct Remote {
    pub store: Arc<Store>,
    pub pool: Arc<Pool>,
}

pub fn global() -> Result<Arc<Remote>, RemoteError> {
    static G: OnceLock<Result<Arc<Remote>, String>> = OnceLock::new();
    G.get_or_init(|| {
        let dir = super::store::data_dir().ok_or_else(|| {
            "no home folder to keep saved connections in; set PS5UPLOAD_DATA_DIR".to_string()
        })?;
        let store = Store::open(&dir).map_err(|e| format!("saved connections: {e}"))?;
        Ok(Arc::new(Remote {
            store: Arc::new(store),
            pool: Arc::new(Pool::new(Box::new(RealConnector))),
        }))
    })
    .clone()
    .map_err(RemoteError::Io)
}

#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn connect(
        &self,
        conn: &Connection,
        secret: &Secret,
    ) -> Result<Arc<dyn RemoteFs>, RemoteError>;
}

/// Signs in with the protocol the connection names.
pub struct RealConnector;

#[async_trait::async_trait]
impl Connector for RealConnector {
    async fn connect(
        &self,
        conn: &Connection,
        secret: &Secret,
    ) -> Result<Arc<dyn RemoteFs>, RemoteError> {
        match conn.protocol {
            Protocol::Smb => Ok(Arc::new(super::smb_fs::SmbFs::connect(conn, secret).await?)),
            Protocol::Ftp => Ok(Arc::new(
                super::ftp_fs::FtpFs::connect(conn, secret, false).await?,
            )),
            Protocol::Ftps => Ok(Arc::new(
                super::ftp_fs::FtpFs::connect(conn, secret, true).await?,
            )),
            Protocol::Sftp => Err(RemoteError::Io("SFTP servers are not available yet".into())),
        }
    }
}

/// Connection id → (its session, when it was last used).
type Sessions = HashMap<String, (Arc<dyn RemoteFs>, Instant)>;

pub struct Pool {
    connector: Box<dyn Connector>,
    entries: Mutex<Sessions>,
    connects: AtomicUsize,
}

impl Pool {
    pub fn new(connector: Box<dyn Connector>) -> Self {
        Self {
            connector,
            entries: Mutex::new(HashMap::new()),
            connects: AtomicUsize::new(0),
        }
    }

    pub fn connector(&self) -> &dyn Connector {
        self.connector.as_ref()
    }

    /// Sign-ins so far (tests use it to see a session reused or dropped).
    pub fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    /// The session for connection `id`, signing in if there is none (or it went idle).
    pub async fn fs(&self, store: &Store, id: &str) -> Result<Arc<dyn RemoteFs>, RemoteError> {
        let Some((conn, secret)) = store.get(id) else {
            self.invalidate(id);
            return Err(RemoteError::UnknownConnection(id.to_string()));
        };
        {
            let mut entries = self.lock();
            let now = Instant::now();
            entries.retain(|_, (_, used)| now.duration_since(*used) < IDLE);
            if let Some((fs, used)) = entries.get_mut(id) {
                *used = now;
                return Ok(Arc::clone(fs));
            }
        }
        let fs = self
            .connector
            .connect(&conn, &secret)
            .await
            .map_err(|e| scrub(e, &secret))?;
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.lock()
            .insert(id.to_string(), (Arc::clone(&fs), Instant::now()));
        Ok(fs)
    }

    /// Forget the session: the connection was edited or deleted, or the session broke.
    pub fn invalidate(&self, id: &str) {
        self.lock().remove(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Sessions> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Delays between read attempts.
#[derive(Clone, Debug)]
pub struct Backoff(pub Vec<Duration>);

impl Backoff {
    /// Three retries within 30 s.
    pub fn standard() -> Self {
        Self(vec![
            Duration::from_secs(1),
            Duration::from_secs(4),
            Duration::from_secs(10),
        ])
    }
    pub fn instant() -> Self {
        Self(vec![Duration::ZERO; 3])
    }
}

/// A file whose reads survive a dropped connection: on a network error it waits, signs in
/// again through the pool, reopens the file and retries. A sign-in failure is never retried.
pub fn retrying(
    pool: Arc<Pool>,
    store: Arc<Store>,
    id: String,
    path: String,
    file: Arc<dyn RemoteFile>,
    backoff: Backoff,
) -> Arc<dyn RemoteFile> {
    Arc::new(Retrying {
        size: file.size(),
        inner: Mutex::new(file),
        pool,
        store,
        id,
        path,
        backoff,
    })
}

struct Retrying {
    size: u64,
    inner: Mutex<Arc<dyn RemoteFile>>,
    pool: Arc<Pool>,
    store: Arc<Store>,
    id: String,
    path: String,
    backoff: Backoff,
}

impl Retrying {
    fn current(&self) -> Arc<dyn RemoteFile> {
        Arc::clone(&self.inner.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[async_trait::async_trait]
impl RemoteFile for Retrying {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        let mut last = match self.current().read_at(offset, len).await {
            Ok(b) => return Ok(b),
            Err(e) => e,
        };
        for delay in &self.backoff.0 {
            if !is_transient(&last) {
                return Err(last);
            }
            tokio::time::sleep(*delay).await;
            self.pool.invalidate(&self.id);
            let reopened = match self.pool.fs(&self.store, &self.id).await {
                Ok(fs) => fs.open(&self.path).await,
                Err(e) => Err(e),
            };
            match reopened {
                Ok(f) => {
                    *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&f);
                    match f.read_at(offset, len).await {
                        Ok(b) => return Ok(b),
                        Err(e) => last = e,
                    }
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// A dropped connection or a server that went away for a moment — worth another try.
fn is_transient(e: &RemoteError) -> bool {
    matches!(e, RemoteError::Io(_) | RemoteError::Unreachable(_))
}

/// Replace any secret the server echoed back in an error with "***".
pub fn scrub(e: RemoteError, secret: &Secret) -> RemoteError {
    let clean = |mut m: String| {
        for s in secret.sensitive() {
            m = m.replace(s, "***");
        }
        m
    };
    match e {
        RemoteError::BadPath(m) => RemoteError::BadPath(clean(m)),
        RemoteError::UnknownConnection(m) => RemoteError::UnknownConnection(m),
        RemoteError::Auth(m) => RemoteError::Auth(clean(m)),
        RemoteError::Unreachable(m) => RemoteError::Unreachable(clean(m)),
        RemoteError::NotFound(m) => RemoteError::NotFound(clean(m)),
        RemoteError::Io(m) => RemoteError::Io(clean(m)),
        e @ RemoteError::HostKey { .. } => e,
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::remote::MemFs;

    /// Hands out one shared MemFs, or fails as told.
    pub struct FakeConnector {
        pub fs: Arc<MemFs>,
        pub fail: Option<fn(&Secret) -> RemoteError>,
    }

    #[async_trait::async_trait]
    impl Connector for FakeConnector {
        async fn connect(
            &self,
            _conn: &Connection,
            secret: &Secret,
        ) -> Result<Arc<dyn RemoteFs>, RemoteError> {
            if let Some(f) = self.fail {
                return Err(f(secret));
            }
            Ok(Arc::new(SharedMem(Arc::clone(&self.fs))))
        }
    }

    struct SharedMem(Arc<MemFs>);

    #[async_trait::async_trait]
    impl RemoteFs for SharedMem {
        async fn list(
            &self,
            p: &str,
            c: Option<String>,
        ) -> Result<crate::remote::Page, RemoteError> {
            self.0.list(p, c).await
        }
        async fn stat(&self, p: &str) -> Result<crate::remote::Entry, RemoteError> {
            self.0.stat(p).await
        }
        async fn open(&self, p: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
            self.0.open(p).await
        }
        async fn walk(
            &self,
            p: &str,
            l: usize,
        ) -> Result<Vec<(String, crate::remote::Entry)>, RemoteError> {
            self.0.walk(p, l).await
        }
    }

    /// A Remote over a fresh temp store and `fs`.
    pub fn remote_with(fs: MemFs, fail: Option<fn(&Secret) -> RemoteError>) -> Arc<Remote> {
        remote_with_shared(Arc::new(fs), fail)
    }

    /// Same, keeping a handle on the MemFs (to make its reads fail).
    pub fn remote_with_shared(
        fs: Arc<MemFs>,
        fail: Option<fn(&Secret) -> RemoteError>,
    ) -> Arc<Remote> {
        let store = Store::open(&crate::remote::store::test_dir()).unwrap();
        Arc::new(Remote {
            store: Arc::new(store),
            pool: Arc::new(Pool::new(Box::new(FakeConnector { fs, fail }))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::remote_with;
    use super::*;
    use crate::remote::store::conn;
    use crate::remote::MemFs;

    #[tokio::test]
    async fn reuses_a_session_and_drops_it_on_invalidate() {
        let r = remote_with(MemFs::new(&[("/a", b"x")]), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        r.pool.fs(&r.store, &id).await.unwrap();
        r.pool.fs(&r.store, &id).await.unwrap();
        assert_eq!(r.pool.connects(), 1);
        r.pool.invalidate(&id);
        r.pool.fs(&r.store, &id).await.unwrap();
        assert_eq!(r.pool.connects(), 2);
        assert!(matches!(
            r.pool.fs(&r.store, "gone-0000").await,
            Err(RemoteError::UnknownConnection(_))
        ));
    }

    #[tokio::test]
    async fn a_retrying_file_survives_dropped_reads_but_not_a_lost_server() {
        let mem = Arc::new(MemFs::new(&[("/a", b"hello")]));
        let r = testing::remote_with_shared(Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let raw = r
            .pool
            .fs(&r.store, &id)
            .await
            .unwrap()
            .open("/a")
            .await
            .unwrap();
        let f = retrying(
            Arc::clone(&r.pool),
            Arc::clone(&r.store),
            id.clone(),
            "/a".into(),
            raw,
            Backoff::instant(),
        );
        mem.fail_next_reads(3);
        assert_eq!(f.read_at(0, 5).await.unwrap(), b"hello");
        mem.fail_next_reads(4);
        assert!(f.read_at(0, 5).await.is_err());
    }
}
