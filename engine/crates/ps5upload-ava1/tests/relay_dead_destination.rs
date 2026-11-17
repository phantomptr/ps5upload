//! A destination that dies while its readers wait on a slow source. Its own process,
//! apart from relay_order.rs: that file turns the relay's no-progress bound (a
//! process-wide knob) down to 5 s, and run alongside it this test's relay gave up
//! before its first byte reached the destination.
mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use ava1::keys::Identity;
use ava1::send::Progress;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::*;
use ps5upload_ava1::gen;
use ps5upload_ava1::relay::ps5_to_ps5_between;
use ps5upload_ava1::Pool;

/// B's connection dies while its readers are parked waiting for a slow A. The relay
/// must notice the dead session and wake them, not wait for the no-progress bound.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_destination_session_wakes_readers_parked_on_a_slow_source() {
    use std::sync::atomic::Ordering;
    let d = temp("dead-b");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let (a, b) = (d.join("a"), d.join("b"));
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    const BIG: u64 = 12 << 20;
    write_pattern(&a.join("share/src/big"), 8, BIG);
    let (addr_a, addr_b) = (host(&a, key).await, host(&b, key).await);
    // A is slow: B's readers are almost always waiting for data.
    let slow = Arc::new(
        ChaosProxy::start(
            addr_a.parse().unwrap(),
            ChaosConfig {
                bytes_per_sec: Some(1 << 20),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let to_b = Arc::new(
        ChaosProxy::start(addr_b.parse().unwrap(), ChaosConfig::default())
            .await
            .unwrap(),
    );
    let pa = Pool::new(ava.clone()).with_addr(slow.addr.to_string());
    let pb = Pool::new(ava).with_addr(to_b.addr.to_string());
    let progress = Arc::new(Progress::default());
    let p2 = progress.clone();
    let run = tokio::task::spawn_blocking(move || {
        ps5_to_ps5_between(
            &pa,
            "a",
            "src",
            &pb,
            "b",
            "dst",
            [32; 16],
            p2,
            Arc::new(AtomicBool::new(false)),
        )
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        while progress.bytes_sent.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no bytes reached the destination");
    to_b.kill_all();
    // Only the attempt B died under needs the slow source. Once the relay has noticed and
    // opened a new connection to B (which takes the wake this test is about: with A still
    // slow, the parked readers would otherwise sit out the bound), the rest runs at full
    // speed instead of crawling through the remaining megabytes at 1 MiB/s.
    let unthrottle = {
        let (slow, to_b) = (slow.clone(), to_b.clone());
        tokio::spawn(async move {
            while to_b.connections() == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            slow.set_bytes_per_sec(None);
        })
    };
    // Far below the 120 s no-progress bound.
    let report = tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("the relay waited out the no-progress bound after the destination died")
        .unwrap()
        .unwrap();
    unthrottle.abort();
    assert_eq!(report.status, gen::STATUS_OK);
    assert_pattern(&b.join("share/dst/big"), 8, BIG);
}
