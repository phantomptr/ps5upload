//! A resume right after a cancel is answered promptly. An upload behind a throttled link is
//! cancelled, the caller re-runs the same job id on the same shared session at once, and the
//! new JobOpen must be answered (OK or BUSY, never silence) so the upload finishes, resending
//! nothing that was already durable. Both receivers: the Rust test server and the C one.
#![cfg(unix)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::{dir, SECRET};
use ps5upload_ava1::upload::upload_dir_in;
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

const TOTAL: usize = 6 << 20;

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1:9113");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

fn source(t: &Path) -> PathBuf {
    let src = t.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..2u8 {
        let b: Vec<u8> = (0..TOTAL / 2)
            .map(|j| (j as u8).wrapping_mul(7).wrapping_add(i))
            .collect();
        std::fs::write(src.join(format!("f{i}")), b).unwrap();
    }
    src
}

/// Cancel mid-transfer, resume at once, and demand an answer within the bound.
async fn scenario(tag: &str, c_receiver: bool) {
    let t = dir(tag);
    let ava = t.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let (addr, dest, _srv): (String, String, Option<CServer>) = if c_receiver {
        PeerStore::load(&t.join("srv-peers"))
            .unwrap()
            .add(me.public(), "engine")
            .unwrap();
        PeerStore::load(&ava.join("peers"))
            .unwrap()
            .add(Identity::from_secret(SECRET).public(), "C receiver")
            .unwrap();
        let s = CServer::start_data(
            SECRET,
            &t.join("srv-peers"),
            &t.join("jobs"),
            200,
            2000,
            2000,
            0,
        );
        (s.addr(), t.join("dest").to_str().unwrap().into(), Some(s))
    } else {
        let mut peers = PeerStore::in_memory();
        peers.add(me.public(), "engine").unwrap();
        let rpc: server::RpcHandler = Box::new(|method, _| {
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
        });
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc).with_jobs(
            Arc::new(FolderHost {
                root: t.join("share"),
                jobs_dir: t.join("hjobs"),
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        (a, "out".into(), None)
    };
    let proxy = ChaosProxy::start(
        addr.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(256 << 10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pool = Arc::new(Pool::new(ava).with_addr(proxy.addr.to_string()));
    let src = source(&t);
    let id = [0x5a; 16];

    // First run: cancel once some bytes are on the wire.
    let c1 = cfg();
    let (flag, sent, durable) = (
        c1.cancel.clone().unwrap(),
        c1.progress_bytes.clone().unwrap(),
        c1.progress_bytes_finalized.clone().unwrap(),
    );
    let (p, s, d) = (pool.clone(), src.clone(), dest.clone());
    let first = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c1, id, &d, &s));
    let start = std::time::Instant::now();
    while sent.load(Ordering::Relaxed) < (1 << 20) {
        assert!(start.elapsed() < Duration::from_secs(60), "no progress");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    flag.store(true, Ordering::Relaxed);
    let e = tokio::time::timeout(Duration::from_secs(60), first)
        .await
        .expect("the cancelled upload never returned")
        .unwrap()
        .expect_err("cancelled");
    assert!(e.to_string().contains("cancel"), "{e:#}");
    let durable_at_cancel = durable.load(Ordering::Relaxed);

    // Resume immediately, same job id, same pool (same shared session).
    let c2 = cfg();
    let sent2 = c2.progress_bytes.clone().unwrap();
    let flag2 = c2.cancel.clone().unwrap();
    let (p, s, d) = (pool.clone(), src.clone(), dest.clone());
    let second = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c2, id, &d, &s));
    // The regression this pins is a JobOpen that never answers, so the resume must start
    // moving bytes within 30 s; finishing the rest through the 256 KiB/s proxy takes 20 s+
    // on its own, so it gets a separate, generous budget.
    let start = std::time::Instant::now();
    while sent2.load(Ordering::Relaxed) == 0 && !second.is_finished() {
        if start.elapsed() > Duration::from_secs(30) {
            flag2.store(true, Ordering::Relaxed); // let the blocked thread end so the runtime can drop
            panic!("the resume was not answered in 30 s (JobOpen hung)");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let r = tokio::time::timeout(Duration::from_secs(120), second).await;
    let Ok(r) = r else {
        flag2.store(true, Ordering::Relaxed);
        panic!(
            "the resume started but did not finish in 120 s: {} bytes sent ({} durable at cancel)",
            sent2.load(Ordering::Relaxed),
            durable_at_cancel
        );
    };
    let r = r
        .unwrap()
        .unwrap_or_else(|e| panic!("resume failed: {e:#}"));
    assert!(r.bytes_sent as usize <= TOTAL, "{}", r.bytes_sent);
    // Cancel keeps the durable state: a resume continues from it and never resends it.
    assert!(
        sent2.load(Ordering::Relaxed) + durable_at_cancel <= TOTAL as u64,
        "resent durable data: sent {} after {} durable",
        sent2.load(Ordering::Relaxed),
        durable_at_cancel
    );
    let root = if c_receiver {
        PathBuf::from(&dest)
    } else {
        t.join("share/out")
    };
    for i in 0..2u8 {
        let want: Vec<u8> = (0..TOTAL / 2)
            .map(|j| (j as u8).wrapping_mul(7).wrapping_add(i))
            .collect();
        assert!(
            std::fs::read(root.join(format!("f{i}"))).unwrap() == want,
            "f{i}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_receiver_answers_a_resume_right_after_a_cancel() {
    scenario("resume-cancel-rust", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn c_receiver_answers_a_resume_right_after_a_cancel() {
    scenario("resume-cancel-c", true).await;
}
