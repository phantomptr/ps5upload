//! The AVA1 test console shared by the `ava1_*` integration tests (the replacement for the
//! retired mock server): the Rust job host over a temp folder for uploads and downloads, and a
//! scripted management node for the hardware/system methods.
//!
//! Everything runs on loopback and every wait is bounded.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::gen::{self, MgmtText};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ps5upload_ava1::mgmt::AvaTransport;
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

/// A temp directory that removes itself.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn tempdir() -> TempDir {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("ps5u-ava1t-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

/// A deterministic job id.
pub fn job_id(seed: u8) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(seed);
    }
    id
}

pub fn cfg() -> TransferConfig {
    // The address is only the pool's key: the pool resolves it to the loopback console.
    TransferConfig::new("127.0.0.1")
}

/// A console that accepts uploads into (and serves downloads from) `share/`, and the engine's pool
/// aimed at it. The console trusts the engine's key, as a launched helper would.
pub struct Console {
    pub dir: TempDir,
    pub addr: String,
    pub pool: Arc<Pool>,
    pub share: PathBuf,
    /// Held by the rate-limited consoles: they take seconds each, and run side by side on a
    /// loaded host they starve one another past any sensible bound, so they take turns.
    pub _turn: Option<tokio::sync::MutexGuard<'static, ()>>,
}

/// The CPU-heavy tests (loops of uploads, throughput gates) take the same turn as the
/// rate-limited consoles: a debug build saturating the cores starves a throttled transfer past
/// any bound a test can sensibly set.
pub async fn heavy() -> tokio::sync::MutexGuard<'static, ()> {
    HEAVY_TURN.lock().await
}

/// The rate-limited consoles take turns (see `Console::_turn`); so do the CPU-heavy tests (`heavy`).
static HEAVY_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn console() -> Console {
    let dir = tempdir();
    let (addr, pool) = start_host(dir.path(), None).await;
    Console {
        share: dir.path().join("share"),
        dir,
        addr,
        pool: Arc::new(pool),
        _turn: None,
    }
}

/// Starts a folder host under `dir` and returns its address and a pool aimed at `via`
/// (the host itself unless a chaos proxy sits in between).
pub async fn start_host(dir: &Path, via: Option<String>) -> (String, Pool) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(Arc::new(FolderHost {
        root: dir.join("share"),
        jobs_dir: dir.join("hjobs"),
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool = Pool::new(ava).with_addr(via.unwrap_or_else(|| addr.clone()));
    (addr, pool)
}

/// Only `node.info` is answered; everything else is `ERR_UNKNOWN_METHOD`.
pub fn node_info_rpc() -> RpcHandler {
    Box::new(|method, _| {
        if method == gen::METHOD_NODE_INFO {
            let info = gen::NodeInfo {
                version: "test".into(),
                platform: "rust".into(),
                name: "host".into(),
                firmware: None,
            };
            RpcReply {
                status: gen::STATUS_OK,
                body: info.to_bytes().unwrap(),
            }
        } else {
            RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: Vec::new(),
            }
        }
    })
}

/// Runs blocking work (the upload adapters block) off the reactor, bounded.
pub async fn run<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::time::timeout(Duration::from_secs(secs), tokio::task::spawn_blocking(f))
        .await
        .expect("timed out")
        .expect("worker panicked")
}

/// Every regular file under `root` as `relative/path -> bytes` (forward slashes).
pub fn landed(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A position-dependent byte pattern: a wrong offset, a swapped file or a withheld range
/// changes the content.
pub fn pattern(seed: usize, n: usize) -> Vec<u8> {
    (0..n)
        .map(|j| ((seed * 131 + j * 7 + (j >> 8) * 13) & 0xff) as u8)
        .collect()
}

// ─── the scripted management node ──────────────────────────────────────────────

/// A management reply carrying legacy text.
pub fn text(s: &str) -> RpcReply {
    RpcReply {
        status: gen::STATUS_OK,
        body: MgmtText {
            body: s.as_bytes().to_vec(),
            more: None,
        }
        .to_bytes()
        .unwrap(),
    }
}

/// A refusal whose body is the handler's legacy token.
pub fn refuse(status: u16, cause: &str) -> RpcReply {
    RpcReply {
        status,
        body: cause.as_bytes().to_vec(),
    }
}

/// The text of a `MgmtText` request.
pub fn text_of(req: &[u8]) -> String {
    String::from_utf8(MgmtText::decode(req).map(|t| t.body).unwrap_or_default()).unwrap_or_default()
}

/// A loopback management node running `handler`, with the transport over its own pool. The node
/// lives on its own runtime thread so synchronous tests can call the core functions directly
/// (the transport is installed per thread with `scoped_transport`).
pub struct Node {
    pub transport: Arc<AvaTransport>,
    _rt: tokio::runtime::Runtime,
    _dir: TempDir,
}

impl Node {
    pub fn start(handler: RpcHandler) -> Node {
        let dir = tempdir();
        let ava = dir.path().join("ava");
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        let mut peers = PeerStore::in_memory();
        peers.add(me.public(), "engine").unwrap();
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "node", peers, handler).with_mgmt();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let l = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = l.local_addr().unwrap().to_string();
        rt.spawn(server::serve(l, Arc::new(ctx)));
        let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
        Node {
            transport: Arc::new(AvaTransport::with_pool(pool)),
            _rt: rt,
            _dir: dir,
        }
    }

    /// Installs this node's transport for the calling thread until the guard drops.
    pub fn attach(&self) -> ps5upload_core::mgmt::ScopedTransport {
        ps5upload_core::mgmt::scoped_transport(self.transport.clone())
    }
}

/// The console name the core functions are given: only a pool key.
pub const CONSOLE: &str = "127.0.0.1";

// ─── cancel and slow links ─────────────────────────────────────────────────────

/// Starts `f` (an upload with `config.cancel` armed), waits for durable progress, cancels it, and
/// returns what it answered. The same job id is then re-run by the caller.
pub async fn cancel_midway<T: Send + 'static>(
    proxy: &ChaosProxy,
    config: &TransferConfig,
    f: impl FnOnce(TransferConfig) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Error {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finalized = Arc::new(AtomicU64::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let mut c = config.clone();
    c.cancel = Some(flag.clone());
    c.progress_bytes_finalized = Some(finalized.clone());
    c.progress_bytes = Some(sent.clone());
    let worker = tokio::task::spawn_blocking(move || f(c));
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    // Cancel once something is durable, or once a fraction of the transfer is on the wire
    // (durable progress is journaled in batches and can arrive late on a loaded host).
    while finalized.load(Ordering::Relaxed) == 0
        && sent.load(Ordering::Relaxed) < (3 << 20)
        && !worker.is_finished()
    {
        assert!(std::time::Instant::now() < deadline, "no durable progress");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    flag.store(true, Ordering::Relaxed);
    let r = tokio::time::timeout(Duration::from_secs(180), worker)
        .await
        .expect("the cancelled upload never returned")
        .unwrap();
    // A resume under the same job id follows at once: a JobOpen for a job the receiver still
    // holds is answered BUSY and retried by the sender (resume-after-cancel fix, 38676afe).
    let _ = proxy;
    match r {
        Err(e) => e,
        Ok(_) => panic!("the upload finished before the cancel landed (file too small?)"),
    }
}

/// A source big enough, written slowly enough through a capped proxy, that a cancel lands
/// mid-transfer. Returns the console behind a rate-limited proxy.
pub async fn slow_console() -> (Console, Arc<ChaosProxy>) {
    let turn = HEAVY_TURN.lock().await;
    let dir = tempdir();
    let (addr, pool) = start_host(dir.path(), None).await;
    let proxy = Arc::new(
        ChaosProxy::start(
            addr.parse().unwrap(),
            ChaosConfig {
                // per connection, and an upload spreads over several lanes: slow enough that a 12 MiB
                // upload takes seconds, so a cancel or a kill lands mid-transfer.
                bytes_per_sec: Some(256 << 10),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let pool = Arc::new(pool.with_addr(proxy.addr.to_string()));
    (
        Console {
            share: dir.path().join("share"),
            dir,
            addr,
            pool,
            _turn: Some(turn),
        },
        proxy,
    )
}

// ─── fixtures ──────────────────────────────────────────────────────────────────

pub fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A realistic game-dump folder (the old `build_game_folder`): real files plus OS junk the
/// default excludes must drop. Returns the real files only.
pub fn build_game_folder(root: &Path) -> Vec<(String, Vec<u8>)> {
    let real: Vec<(&str, usize)> = vec![
        ("eboot.bin", 20_000),
        ("sce_sys/param.json", 800),
        ("sce_sys/icon0.png", 0),
        ("sce_sys/about/right.sprx", 1),
        ("Image0/data/big.dat", 30_000),
        ("Image0/deep/nested/dir/leaf.dat", 5_000),
        ("name with spaces.bin", 600),
        ("unicodé_名前.bin", 700),
        ("dots...in.name", 333),
    ];
    let junk: Vec<(&str, usize)> = vec![
        ("._eboot.bin", 4096),
        (".DS_Store", 6148),
        ("sce_sys/._param.json", 100),
        ("Thumbs.db", 200),
    ];
    let mut expected = Vec::new();
    for (i, (rel, sz)) in real.iter().enumerate() {
        let bytes = pattern(i + 1, *sz);
        write_file(&root.join(rel), &bytes);
        expected.push((rel.to_string(), bytes));
    }
    for (rel, sz) in &junk {
        write_file(&root.join(rel), &vec![0xAAu8; *sz]);
    }
    expected
}

// ─── a host that accepts absolute roots ────────────────────────────────────────

/// A job host that, unlike `FolderHost`, accepts an absolute `JobOpen.root` (the console does:
/// its write policy, not the path shape, decides). The root is mapped under `share/`
/// (`/data/other` becomes `share/data/other`) and recorded, so a test can see how many jobs
/// a file list became and under which roots. A root starting with `/forbidden` is refused.
pub struct MappedHost {
    pub inner: FolderHost,
    pub roots: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ava1::router::JobHost for MappedHost {
    fn accept(&self, link: ava1::router::JobLink, mut first: ava1::conn::Frame, peer: [u8; 32]) {
        if first.ty == <gen::JobOpen as ava1::wire::FrameMessage>::TYPE {
            if let Ok(mut open) = first.decode::<gen::JobOpen>() {
                self.roots.lock().unwrap().push(open.root.clone());
                if open.root.starts_with("/forbidden") {
                    tokio::spawn(async move {
                        let _ = link
                            .control
                            .send(&gen::JobOpenAck {
                                job_id: open.job_id,
                                status: gen::ERR_PATH,
                                credit: 0,
                                staged: 0,
                                workers: 0,
                                message: Some("the write policy refuses this root".into()),
                            })
                            .await;
                    });
                    return;
                }
                open.root = open.root.trim_start_matches('/').to_string();
                first.body = open.to_bytes().unwrap();
            }
        }
        self.inner.accept(link, first, peer);
    }
}

/// A console on a [`MappedHost`], optionally behind a rate-limited proxy.
pub struct AbsConsole {
    pub console: Console,
    pub roots: Arc<std::sync::Mutex<Vec<String>>>,
    pub proxy: Option<Arc<ChaosProxy>>,
}

pub async fn abs_console(slow: bool) -> AbsConsole {
    let turn = if slow {
        Some(HEAVY_TURN.lock().await)
    } else {
        None
    };
    let dir = tempdir();
    let ava = dir.path().join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let roots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let host = MappedHost {
        inner: FolderHost {
            root: dir.path().join("share"),
            jobs_dir: dir.path().join("hjobs"),
        },
        roots: roots.clone(),
    };
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(Arc::new(host));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let (via, proxy) = if slow {
        let p = Arc::new(
            ChaosProxy::start(
                addr.parse().unwrap(),
                ChaosConfig {
                    bytes_per_sec: Some(256 << 10),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        (p.addr.to_string(), Some(p))
    } else {
        (addr.clone(), None)
    };
    let pool = Arc::new(Pool::new(ava).with_addr(via));
    AbsConsole {
        console: Console {
            share: dir.path().join("share"),
            dir,
            addr,
            pool,
            _turn: turn,
        },
        roots,
        proxy,
    }
}
