//! A folder download of more small files than the process may hold descriptors must finish:
//! the receiver keeps a bounded number of files open (final review: engine #1). Its own test
//! binary because it lowers `RLIMIT_NOFILE` for the whole process.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::download::{self, Counters};
use ps5upload_ava1::Pool;
use ps5upload_core::download::DownloadKind;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-fds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn node_info() -> RpcHandler {
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

fn count_fds() -> usize {
    std::fs::read_dir(if Path::new("/dev/fd").exists() {
        "/dev/fd"
    } else {
        "/proc/self/fd"
    })
    .map(|r| r.count())
    .unwrap_or(0)
}

/// The rlimit and the environment are the process's: one scenario at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_of_more_files_than_descriptors_downloads() {
    scenario("many", "1", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_without_the_pack_log() {
    scenario("many-nolog", "0", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn running_out_of_descriptors_mid_download_is_waited_out() {
    scenario("many-hog", "0", true).await;
}

async fn scenario(tag: &str, log_small: &str, hog: bool) {
    let _one = ONE_AT_A_TIME.lock().await;
    std::env::set_var("PS5UPLOAD_AVA1_LOG_SMALL", log_small);
    let d = temp(tag);
    let n = 700usize;
    for i in 0..n {
        let p = d.join(format!("share/Game/d{}/f{i}", i % 9));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("file number {i} ").repeat(1 + i % 5)).unwrap();
    }
    let ava = d.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let id = Identity::load_or_create(&d.join("srv-identity")).unwrap();
    let ctx = ServerCtx::new(id, "host", peers, node_info()).with_jobs(Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool = Pool::new(ava).with_addr(&addr);

    // Headroom for the runtime, the sockets and the host's own files, far below `n`.
    let limit = (count_fds() + 120) as libc::rlim_t;
    let mut old = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut old), 0);
        let new = libc::rlimit {
            rlim_cur: limit,
            rlim_max: old.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &new), 0);
    }
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = Counters {
        bytes: Arc::default(),
        files: Arc::default(),
        files_finalized: Arc::default(),
        bytes_finalized: Arc::default(),
        total: Some(Arc::default()),
    };
    // A hog takes every free descriptor for a moment once bytes are arriving, then lets go: the
    // sink's next open fails with EMFILE and the download must wait and finish.
    let hog = hog.then(|| {
        let bytes = c.bytes.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            while bytes.load(std::sync::atomic::Ordering::Relaxed) == 0
                && t.elapsed() < Duration::from_secs(20)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut held = Vec::new();
            while let Ok(f) = std::fs::File::open("/dev/null") {
                held.push(f);
            }
            std::thread::sleep(Duration::from_millis(1500));
            drop(held);
        })
    });
    let (o2, c2) = (out.clone(), c.clone());
    let r = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            download::to_local_in(
                &pool,
                "console",
                "Game",
                DownloadKind::Folder,
                &o2,
                false,
                [7; 16],
                &c2,
                None,
            )
        }),
    )
    .await
    .expect("timed out")
    .unwrap();
    if let Some(h) = hog {
        h.join().unwrap();
    }
    unsafe {
        libc::setrlimit(libc::RLIMIT_NOFILE, &old);
    }
    r.expect("the download must finish under the descriptor limit");
    let mut seen = 0;
    for i in 0..n {
        let got = std::fs::read(out.join(format!("Game/d{}/f{i}", i % 9))).unwrap();
        assert_eq!(
            got,
            format!("file number {i} ").repeat(1 + i % 5).into_bytes()
        );
        seen += 1;
    }
    assert_eq!(seen, n);
}
