#![cfg(unix)]
//! The open-file budget: tiny-file uploads and disk.calibrate stay within it, and a failure
//! tells the engine which step failed.
mod common;

use ava1::gen;
use ava1::session::connect;
use ava1::wire::Message;
use ava1_ctest::{CServer, LogOpts};
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn two_thousand_tiny_files_never_hold_more_than_the_budget() {
    let d = dir("fd-upload");
    let src = d.join("src");
    write_tree(&src, 2000, |i| 1 + i % 200);
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data_opts(
        SECRET,
        &peers,
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
        0,
        LogOpts::OFF,
    );
    srv.knob("fd_budget", 64);
    srv.knob("fd_peak_reset", 0);
    let root = d.join("dest");
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [7; 16],
        |_| {},
    )
    .await;
    assert_eq!(r.status, 0);
    assert!(same_tree(&src, &root));
    let peak = srv.fd_peak(0);
    assert!(peak > 0 && peak <= 32, "pending fds peaked at {peak}");
}

#[tokio::test(flavor = "multi_thread")]
async fn calibrate_with_many_files_fits_a_small_budget() {
    let d = dir("fd-cal");
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 4000, 4000, 0);
    srv.knob("fd_budget", 64);
    srv.knob("fd_peak_reset", 0);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let cal = d.join("cal");
    std::fs::create_dir_all(&cal).unwrap();
    let pts = s
        .calibrate(cal.to_str().unwrap(), 2000, 4096)
        .await
        .unwrap();
    assert_eq!(pts.len(), 5);
    assert!(pts.iter().all(|p| p.files_per_s > 0), "{pts:?}");
    let peak = srv.fd_peak(1);
    assert!(peak > 0 && peak <= 64, "calibrate held {peak} fds");
    assert_eq!(std::fs::read_dir(&cal).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_calibrate_names_the_step() {
    let d = dir("fd-cal-err");
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 4000, 4000, 0);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let cal = d.join("cal");
    std::fs::create_dir_all(cal.join(".ava-cal-1")).unwrap();
    let err = s
        .calibrate(cal.to_str().unwrap(), 8, 4096)
        .await
        .unwrap_err();
    match err {
        ava1::Ava1Error::Refused { code, message } => {
            assert_eq!(code, gen::ERR_EXISTS);
            assert!(
                message.contains("disk.calibrate") && message.contains("mkdir"),
                "{message}"
            );
            assert!(message.contains("exist"), "{message}");
        }
        e => panic!("{e:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_calibrate_on_a_missing_dir_carries_its_cause_to_the_session() {
    let d = dir("fd-cal-missing");
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 4000, 4000, 0);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let missing = d.join("nope/deeper");
    // The raw reply: a non-OK status still carries the console's text.
    let body = gen::DiskCalibrate {
        dir: missing.to_str().unwrap().into(),
        files: 8,
        size: 4096,
    }
    .to_bytes()
    .unwrap();
    let reply = s.rpc(gen::METHOD_DISK_CALIBRATE, &body).await.unwrap();
    assert_ne!(reply.status, gen::STATUS_OK);
    assert!(!reply.body.is_empty(), "the error reply dropped its body");
    let err = s
        .calibrate(missing.to_str().unwrap(), 8, 4096)
        .await
        .unwrap_err();
    match err {
        ava1::Ava1Error::Refused { message, .. } => {
            assert!(message.starts_with("disk.calibrate: "), "{message}");
            assert!(message.len() > "disk.calibrate failed".len(), "{message}");
        }
        e => panic!("{e:?}"),
    }
}

/// Final review (console): a peer whose first group covers thousands of large files used to
/// leave two descriptors open per file until its commit, so about 600 of them used the
/// helper's whole table and every accept() and open() failed. Large-file descriptors are
/// capped against the budget now (a quarter of it, half of that per job): idle files are
/// closed and reopened on demand, and a worker with nothing to close waits for the batch.
#[tokio::test(flavor = "multi_thread")]
async fn two_thousand_large_files_stay_within_the_fd_budget() {
    let d = dir("fd-large");
    let src = d.join("src");
    // 256 KiB is the large-file cutoff: each file travels as chunks and holds its own descriptors.
    write_tree(&src, 2000, |_| 256 * 1024);
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data_opts(
        SECRET,
        &peers,
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
        0,
        LogOpts::OFF,
    );
    srv.knob("fd_budget", 64);
    srv.knob("fd_peak_reset", 0);
    let root = d.join("dest");
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [8; 16],
        |_| {},
    )
    .await;
    assert_eq!(r.status, 0);
    assert!(same_tree(&src, &root));
    let peak = srv.fd_peak(2);
    assert!(
        peak > 0 && peak <= 16,
        "large files held {peak} descriptors at once (budget 64, cap 16)"
    );
    // 2 x 500 MiB of scratch: do not leave it behind
    let _ = std::fs::remove_dir_all(&d);
}
