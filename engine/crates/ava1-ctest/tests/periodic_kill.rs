//! Hardware run 2026-10-04 (drop60 rejoin livelock): a proxy that kills every connection on a
//! short period must never strand an upload. Whenever the console is reachable again the job
//! continues from its durable state and ends byte-exact. Both receivers: the C one (what the
//! console runs) and, in `ps5upload-ava1/tests/adapters.rs`, the Rust one.
#![cfg(unix)]
mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::{dir, same_tree, SECRET};
use ps5upload_ava1::upload::upload_dir_in;
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1:9120");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn periodic_kills_never_strand_an_upload_on_the_c_receiver() {
    let t = dir("periodic-kill");
    let ava = t.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    PeerStore::load(&t.join("srv-peers"))
        .unwrap()
        .add(me.public(), "engine")
        .unwrap();
    PeerStore::load(&ava.join("peers"))
        .unwrap()
        .add(Identity::from_secret(SECRET).public(), "C receiver")
        .unwrap();
    let srv = CServer::start_data(
        SECRET,
        &t.join("srv-peers"),
        &t.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(8 << 20),
            kill_every: Some(Duration::from_millis(3000)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let src = t.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let n = 32usize << 20;
    std::fs::write(
        src.join("big.bin"),
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    let pool = Arc::new(Pool::new(ava).with_addr(proxy.addr.to_string()));
    let c = cfg();
    let finalized = c.progress_bytes_finalized.clone().unwrap();
    let dest = t.join("dest");
    let (p, s, d) = (
        pool.clone(),
        src.clone(),
        dest.to_str().unwrap().to_string(),
    );
    tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || upload_dir_in(&p, &c, [0x77; 16], &d, &s)),
    )
    .await
    .expect("the upload never finished under periodic kills")
    .unwrap()
    .unwrap();
    assert!(same_tree(&src, &dest));
    assert_eq!(finalized.load(Ordering::Relaxed), n as u64);
    println!("periodic kills (C): attempts = {}", pool.attempts());
}
