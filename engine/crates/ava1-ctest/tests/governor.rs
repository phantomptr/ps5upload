#![cfg(unix)]
//! End to end receiver measurements. The upload cases use large temporary corpora and run serially.
mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen;
use ava1::send::Progress;
use ava1::session::connect;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::{CServer, LogOpts};
use common::*;

fn watch(pg: Arc<Progress>) -> (Arc<Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sample = seen.clone();
    let handle = tokio::spawn(async move {
        loop {
            sample
                .lock()
                .unwrap()
                .push(pg.bottleneck.load(Ordering::Relaxed));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    (seen, handle)
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_calibrate_reports_five_points_and_cleans_up() {
    let d = dir("gov-cal");
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let cal = d.join("cal");
    std::fs::create_dir_all(&cal).unwrap();
    let pts = s.calibrate(cal.to_str().unwrap(), 400, 4096).await.unwrap();
    assert_eq!(
        pts.iter().map(|p| p.workers).collect::<Vec<_>>(),
        vec![1, 2, 4, 8, 16]
    );
    assert!(pts.iter().all(|p| p.files_per_s > 0), "{pts:?}");
    assert_eq!(std::fs::read_dir(&cal).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_calibrate_respects_write_policy_and_existing_files() {
    let d = dir("gov-cal-deny");
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let cal = d.join("cal");
    std::fs::create_dir_all(cal.join(".ava-cal-1")).unwrap();
    std::fs::write(cal.join(".ava-cal-1/0"), b"keep").unwrap();
    for (files, size) in [(0, 4096), (20_001, 4096), (8, (1 << 20) + 1)] {
        let err = s
            .calibrate(cal.to_str().unwrap(), files, size)
            .await
            .unwrap_err();
        assert!(matches!(err, ava1::Ava1Error::Refused { code, .. } if code == gen::ERR_PROTOCOL));
    }
    let err = s
        .calibrate(cal.to_str().unwrap(), 8, 4096)
        .await
        .unwrap_err();
    assert!(matches!(err, ava1::Ava1Error::Refused { .. }), "{err:?}");
    assert_eq!(std::fs::read(cal.join(".ava-cal-1/0")).unwrap(), b"keep");
    std::fs::remove_dir_all(cal.join(".ava-cal-1")).unwrap();
    srv.knob("deny_write", 1);
    let err = s
        .calibrate(cal.to_str().unwrap(), 8, 4096)
        .await
        .unwrap_err();
    srv.knob("deny_write", 0);
    assert!(
        matches!(err, ava1::Ava1Error::Refused { code, .. } if code == gen::ERR_PATH),
        "{err:?}"
    );
    assert_eq!(std::fs::read_dir(&cal).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_disk_is_reported_as_the_bottleneck() {
    let d = dir("gov-disk");
    let src = d.join("src");
    write_tree(&src, 6000, |_| 4096);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data_opts(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        20000,
        0,
        LogOpts::OFF,
    );
    srv.knob("budget_free", 16 << 20);
    let pg = Arc::new(Progress::default());
    let (seen, watcher) = watch(pg.clone());
    let (report, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        d.join("dst").to_str().unwrap(),
        [0x81; 16],
        move |opts| opts.progress = pg.clone(),
    )
    .await;
    watcher.abort();
    assert_eq!(report.status, 0, "{:?}", report.message);
    // Under the sanitizers (the C) or coverage instrumentation (the Rust sender) one side runs
    // several times slower and becomes the bottleneck itself: the classification only means
    // something at native speed. The transfer must still succeed.
    // On a shared CI runner (CI set) the debug Rust sender is slower than the C receiver's
    // 20 ms-per-fsync disk, so the receiver keeps up and truthfully reports the network: the
    // label then describes the runner, not the code. It is checked locally and in `make test`.
    if cfg!(ava1_ctest_sanitize)
        || std::env::var_os("CARGO_LLVM_COV").is_some()
        || std::env::var_os("CI").is_some()
    {
        return;
    }
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|b| *b == gen::BN_DISK || *b == gen::BN_WORKERS),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rate_capped_link_is_reported_as_the_network() {
    let d = dir("gov-net");
    let file = d.join("big.bin");
    std::fs::write(&file, vec![7u8; 48 << 20]).unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
    );
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(8 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pg = Arc::new(Progress::default());
    let (seen, watcher) = watch(pg.clone());
    let (report, _) = upload(
        &proxy.addr.to_string(),
        me,
        mine,
        &file,
        d.join("o.bin").to_str().unwrap(),
        [0x82; 16],
        move |opts| opts.progress = pg.clone(),
    )
    .await;
    watcher.abort();
    assert_eq!(report.status, 0, "{:?}", report.message);
    // Under the sanitizers (the C) or coverage instrumentation (the Rust sender) one side runs
    // several times slower and becomes the bottleneck itself: the classification only means
    // something at native speed. The transfer must still succeed.
    if cfg!(ava1_ctest_sanitize) || std::env::var_os("CARGO_LLVM_COV").is_some() {
        return;
    }
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&gen::BN_NETWORK), "{seen:?}");
    assert!(!seen.contains(&gen::BN_DISK), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lanes_grow_on_a_link_that_scales_with_them() {
    let d = dir("gov-lanes");
    let file = d.join("big.bin");
    std::fs::write(&file, vec![9u8; 160 << 20]).unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
    );
    // Chaos caps each connection separately, so additional lanes can increase throughput.
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(4 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pg = Arc::new(Progress::default());
    let progress = pg.clone();
    let proxy_addr = proxy.addr.to_string();
    let dest = d.join("o.bin");
    let transfer = upload(
        &proxy_addr,
        me,
        mine,
        &file,
        dest.to_str().unwrap(),
        [0x83; 16],
        move |opts| opts.progress = progress.clone(),
    );
    let (report, _) = tokio::time::timeout(Duration::from_secs(90), transfer)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "lanes={} sent={} durable={} bn={}",
                pg.lanes.load(Ordering::Relaxed),
                pg.bytes_sent.load(Ordering::Relaxed),
                pg.bytes_durable.load(Ordering::Relaxed),
                pg.bottleneck.load(Ordering::Relaxed)
            )
        });
    assert_eq!(report.status, 0, "{:?}", report.message);
    // Under the sanitizers (the C) or coverage instrumentation (the Rust sender) one side runs
    // several times slower and becomes the bottleneck itself: the classification only means
    // something at native speed. The transfer must still succeed.
    if cfg!(ava1_ctest_sanitize) || std::env::var_os("CARGO_LLVM_COV").is_some() {
        return;
    }
    assert!(report.max_lanes >= 4, "max lanes {}", report.max_lanes);
}
