#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::gen;

pub fn temp(tag: &str) -> PathBuf {
    let d = test_tmp::temp_path(format!("ava1-relay-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn rpc() -> ava1::server::RpcHandler {
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

pub async fn host(root: &Path, engine_key: [u8; 32]) -> String {
    let mut peers = PeerStore::in_memory();
    peers.add(engine_key, "engine").unwrap();
    let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc()).with_jobs(
        Arc::new(FolderHost {
            root: root.join("share"),
            jobs_dir: root.join("jobs"),
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    addr
}

/// A host that can be stopped and started again on the same address with the same
/// journal directory (a console payload restart that keeps its journal).
pub struct Restartable {
    pub addr: String,
    root: PathBuf,
    key: [u8; 32],
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Restartable {
    pub async fn start(root: &Path, engine_key: [u8; 32]) -> Self {
        let mut me = Self {
            addr: "127.0.0.1:0".into(),
            root: root.to_owned(),
            key: engine_key,
            task: None,
        };
        me.up().await;
        me
    }

    async fn up(&mut self) {
        let mut peers = PeerStore::in_memory();
        peers.add(self.key, "engine").unwrap();
        // A stable identity: the engine pins the host's key after the first session.
        let id = Identity::load_or_create(&self.root.join("identity")).unwrap();
        let ctx = ServerCtx::new(id, "host", peers, rpc()).with_jobs(Arc::new(FolderHost {
            root: self.root.join("share"),
            jobs_dir: self.root.join("jobs"),
        }));
        let l = loop {
            match tokio::net::TcpListener::bind(&self.addr).await {
                Ok(l) => break l,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        };
        self.addr = l.local_addr().unwrap().to_string();
        self.task = Some(tokio::spawn(server::serve(l, Arc::new(ctx))));
    }

    /// Stops accepting; callers kill live connections through their proxy.
    pub async fn restart(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
            let _ = t.await;
        }
        self.up().await;
    }
}

/// Byte at `off` of the test file seeded with `seed`: position dependent, so a wrong
/// offset, a withheld range or a stale outboard changes the content.
pub fn byte_at(seed: u8, off: u64) -> u8 {
    let x = off
        .wrapping_add(seed as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((x >> 29) as u8) ^ (off as u8)
}

pub fn pattern(seed: u8, off: u64, len: usize) -> Vec<u8> {
    (0..len as u64).map(|i| byte_at(seed, off + i)).collect()
}

pub fn write_pattern(path: &Path, seed: u8, size: u64) {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut off = 0u64;
    while off < size {
        let n = (size - off).min(1 << 20) as usize;
        f.write_all(&pattern(seed, off, n)).unwrap();
        off += n as u64;
    }
    f.flush().unwrap();
}

/// Streams `path` and compares it with the pattern, with a blake3 over both.
pub fn assert_pattern(path: &Path, seed: u8, size: u64) {
    use std::io::Read;
    assert_eq!(
        std::fs::metadata(path).unwrap().len(),
        size,
        "{path:?} size"
    );
    let mut f = std::fs::File::open(path).unwrap();
    let (mut got, mut want) = (blake3::Hasher::new(), blake3::Hasher::new());
    let mut off = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    while off < size {
        let n = (size - off).min(1 << 20) as usize;
        f.read_exact(&mut buf[..n]).unwrap();
        got.update(&buf[..n]);
        want.update(&pattern(seed, off, n));
        off += n as u64;
    }
    assert_eq!(got.finalize(), want.finalize(), "{path:?} content differs");
}

// Test scratch paths removed when the test process exits (engine/test-support/test_tmp.rs).
#[path = "../../../../test-support/test_tmp.rs"]
mod test_tmp;
