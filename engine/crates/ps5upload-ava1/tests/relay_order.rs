//! Task 24 review, Critical 1: a small file after a large one that takes longer than
//! the relay's no-progress bound must still be relayed. Its own process: the bound is
//! a process-wide knob.
mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::keys::Identity;
use ava1::send::Progress;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::*;
use ps5upload_ava1::relay::{ps5_to_ps5_between, reset_wait_for_tests, set_wait_for_tests};
use ps5upload_ava1::Pool;

#[tokio::test(flavor = "multi_thread")]
async fn a_small_file_after_a_slow_large_one_outlasts_the_wait_bound() {
    // The old design failed the small file once the large one ran past this bound.
    set_wait_for_tests(Duration::from_secs(5));
    let d = temp("order");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let (a, b) = (d.join("a"), d.join("b"));
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    const BIG: u64 = 32 << 20;
    write_pattern(&a.join("share/src/a_big"), 3, BIG);
    write_pattern(&a.join("share/src/z_small"), 4, 5);
    let (addr_a, addr_b) = (host(&a, key).await, host(&b, key).await);
    // Capped so the large file runs well past the knob: ~10.7 s nominal. 4 MiB/s left too
    // little margin — socket buffers absorb a few MiB past the cap (5.9 s seen on CI).
    let proxy = ChaosProxy::start(
        addr_a.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(3 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pa = Pool::new(ava.clone()).with_addr(proxy.addr.to_string());
    let pb = Pool::new(ava).with_addr(addr_b);
    let started = std::time::Instant::now();
    let report = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::task::spawn_blocking(move || {
            ps5_to_ps5_between(
                &pa,
                "a",
                "src",
                &pb,
                "b",
                "dst",
                [31; 16],
                Arc::new(Progress::default()),
                Arc::new(AtomicBool::new(false)),
            )
        }),
    )
    .await
    .expect("relay hung")
    .unwrap();
    reset_wait_for_tests();
    let report = report.unwrap();
    assert!(
        started.elapsed() > Duration::from_secs(6),
        "the large file must outlast the 5 s bound ({:?})",
        started.elapsed()
    );
    assert_eq!(report.status, gen::STATUS_OK);
    assert_pattern(&b.join("share/dst/a_big"), 3, BIG);
    assert_pattern(&b.join("share/dst/z_small"), 4, 5);
}
