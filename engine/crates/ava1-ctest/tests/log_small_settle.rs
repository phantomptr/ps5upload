#![cfg(unix)]
//! The sender's wait for a console to settle (review dbl): it can be cancelled, it fails when the console reports
//! it cannot make files durable, and on timeout it does not report a clean success.
mod common;
use ava1_ctest::TempDir;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::send::{send_job, Progress, SendError, SendOptions, SendReport};
use ava1::session::connect;
use ava1::source::LocalSource;
use ava1_ctest::{sweep_failures, CServer, LogOpts};
use common::*;

async fn send_once(
    tag: &str,
    job: u8,
    setup: impl FnOnce(&mut SendOptions),
) -> (
    Result<SendReport, SendError>,
    Arc<Progress>,
    std::path::PathBuf,
    TempDir,
) {
    let d = dir(tag);
    let src = d.join("src");
    write_tree(&src, 60, |i| 1 + i % 200);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data_opts(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
        0,
        LogOpts::ON,
    );
    let root = d.join("dest");
    std::fs::create_dir_all(&root).unwrap(); // a merge: files settle behind JobDone
    sweep_failures(-1); // the console cannot make its files durable
    let s = LocalSource::new(src.clone());
    let m = ava1::manifest::walk(&s, &|_: &str| false).unwrap();
    let sess = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = sess.job([job; 16]);
    let mut o = SendOptions::upload(root.to_str().unwrap());
    let pg = Arc::new(Progress::default());
    o.progress = pg.clone();
    setup(&mut o);
    let r = tokio::time::timeout(
        Duration::from_secs(60),
        send_job(&mut link, Arc::new(m), Arc::new(s), o),
    )
    .await
    .expect("send_job did not finish");
    sweep_failures(0);
    (r, pg, root, d)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_cannot_settle_fails_the_upload_with_its_reason() {
    let t = Instant::now();
    let (r, _pg, _root, _d) = send_once("settle-fail", 0x81, |_| {}).await;
    match r {
        Err(SendError::Refused { status, message }) => {
            assert_eq!(status, ava1::gen::ERR_IO, "{message}");
            assert!(message.contains("durable"), "{message}");
        }
        other => panic!("expected the console's failure, got {other:?}"),
    }
    assert!(
        t.elapsed() < Duration::from_secs(25),
        "waited out the whole settle window: {:?}",
        t.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settle_wait_that_times_out_is_a_warning_not_a_clean_success() {
    // the console reports its failure only after ~3 s of retries; the sender stops waiting at 700 ms
    let (r, pg, root, _d) = send_once("settle-timeout", 0x82, |o| {
        o.settle_max = Some(Duration::from_millis(700))
    })
    .await;
    let r = r.expect("the bytes are durable in the console's log: the upload succeeded");
    let msg = r.message.expect("a warning");
    assert!(msg.contains("still being made durable"), "{msg}");
    assert!(!pg.settling.load(Ordering::Relaxed));
    assert!(root.join("d00").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_ends_the_settle_wait_at_once_with_a_warning() {
    let t = Instant::now();
    let (r, _pg, _root, _d) = send_once("settle-cancel", 0x83, |o| {
        let c = o.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            c.store(true, Ordering::Relaxed);
        });
    })
    .await;
    let r = r.expect("the upload itself was complete");
    assert!(
        r.message.as_deref().is_some_and(|m| m.contains("cancel")),
        "{:?}",
        r.message
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}
