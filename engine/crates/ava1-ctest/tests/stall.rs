#![cfg(unix)]
//! Review 006 #2 on the console's C receiver: a sender that heartbeats but sends no file data is
//! ended with ERR_STALLED after the progress deadline; a slow-but-moving sender and a slow drive
//! are never cut.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use ava1::conn::Frame;
use ava1::gen::{
    self, Bundle, BundleRecord, Durable, JobDone, JobMap, JobOpen, JobOpenAck, ManifestEnd,
};
use ava1::manifest::{Entry, Manifest};
use ava1::router::{Inbound, JobLink};
use ava1::session::{connect, Session};
use ava1::wire::{FrameMessage, Message};
use ava1_ctest::{CServer, TempDir};
use common::*;

async fn next_control(link: &mut JobLink) -> Frame {
    loop {
        match tokio::time::timeout(Duration::from_secs(20), link.rx.recv())
            .await
            .expect("no frame within 20 s")
            .expect("the job channel closed")
        {
            Inbound::Control(f) if f.ty != gen::Status::TYPE => return f,
            _ => {}
        }
    }
}

fn files(n: u32) -> Manifest {
    Manifest {
        entries: (0..n)
            .map(|i| Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 1,
                path: format!("f{i}"),
                root: None,
            })
            .collect(),
    }
}

fn bundle(job: [u8; 16], id: u32, d: &[u8]) -> Vec<u8> {
    Bundle {
        job_id: job,
        records: vec![BundleRecord {
            file_id: id,
            root: *blake3::hash(d).as_bytes(),
            data: d.to_vec(),
        }],
    }
    .to_bytes()
    .unwrap()
}

/// Opens a job on the C server and returns it ready for data (ack, manifest, map done).
async fn opened(srv: &CServer, d: &Path, ids: Ids, job: [u8; 16], n: u32) -> (JobLink, Session) {
    let s = connect(&srv.addr(), ids.0, ids.1, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job(job);
    link.control
        .send(&JobOpen {
            job_id: job,
            kind: gen::JOB_UPLOAD,
            root: d.join("dest").to_str().unwrap().into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let ack: JobOpenAck = next_control(&mut link).await.decode().unwrap();
    assert_eq!(ack.status, 0);
    let m = files(n);
    for p in m.pages(job) {
        link.control.send(&p).await.unwrap();
    }
    link.control
        .send(&ManifestEnd {
            job_id: job,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    loop {
        let f = next_control(&mut link).await;
        if f.ty == JobMap::TYPE {
            let map: JobMap = f.decode().unwrap();
            assert_eq!(map.status, 0);
            if map.last == 1 {
                break;
            }
        }
    }
    (link, s)
}

type Ids = (
    std::sync::Arc<ava1::keys::Identity>,
    std::sync::Arc<std::sync::Mutex<ava1::peers::PeerStore>>,
);

fn server(tag: &str, delay_us: u32) -> (CServer, TempDir, Ids) {
    let d = dir(tag);
    let ids = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        delay_us,
    );
    (srv, d, ids)
}

async fn job_done(link: &mut JobLink, limit: Duration) -> JobDone {
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < limit, "no JobDone within {limit:?}");
        let f = next_control(link).await;
        if f.ty == JobDone::TYPE {
            return f.decode().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sender_that_only_pings_is_ended_with_err_stalled() {
    let (srv, d, ids) = server("stall-ping", 0);
    srv.knob("progress_ms", 800);
    let (mut link, s) = opened(&srv, &d, ids.clone(), [0x61; 16], 2).await;
    let _lane = link.opener().unwrap().open().await.unwrap();
    let t0 = Instant::now();
    let done = job_done(&mut link, Duration::from_secs(15)).await;
    assert_eq!(done.status, gen::ERR_STALLED, "{:?}", done.message);
    assert!(
        t0.elapsed() >= Duration::from_millis(700),
        "{:?}",
        t0.elapsed()
    );
    assert!(!s.is_closed(), "only the job ended, not the session");
    srv.knob("progress_ms", 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_but_moving_sender_is_not_cut() {
    let (srv, d, ids) = server("stall-moving", 0);
    srv.knob("progress_ms", 800);
    let (mut link, _s) = opened(&srv, &d, ids.clone(), [0x62; 16], 4).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    for i in 0..4u32 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, i + 1, bundle([0x62; 16], i, b"data"))
            .await
            .unwrap();
    }
    let done = job_done(&mut link, Duration::from_secs(30)).await;
    assert_eq!(done.status, 0, "{:?}", done.message);
    assert_eq!(std::fs::read(d.join("dest/f3")).unwrap(), b"data");
    srv.knob("progress_ms", 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_disk_is_not_a_stall() {
    // Every data fsync takes 1.2 s, longer than the 800 ms deadline: the wait on the drive must
    // not count against the sender, which then finishes promptly.
    let (srv, d, ids) = server("stall-disk", 1_200_000);
    srv.knob("progress_ms", 800);
    let (mut link, _s) = opened(&srv, &d, ids.clone(), [0x63; 16], 2).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle([0x63; 16], 0, b"one!"))
        .await
        .unwrap();
    loop {
        if next_control(&mut link).await.ty == Durable::TYPE {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 2, bundle([0x63; 16], 1, b"two!"))
        .await
        .unwrap();
    let done = job_done(&mut link, Duration::from_secs(30)).await;
    assert_eq!(done.status, 0, "{:?}", done.message);
    srv.knob("progress_ms", 0);
}

/// Review 006 follow-up: the long allowance belongs to a job that resumed, decided at open, not to
/// any job that has made one durable batch. A fresh job whose source wedges after its first file is
/// cut at the fresh limit although the resume limit is huge.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_job_that_made_one_batch_still_gets_the_fresh_limit() {
    let (srv, d, ids) = server("stall-fresh-batch", 0);
    srv.knob("progress_ms", 800);
    srv.knob("resume_progress_ms", 120_000);
    let (mut link, _s) = opened(&srv, &d, ids.clone(), [0x64; 16], 3).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle([0x64; 16], 0, b"one!"))
        .await
        .unwrap();
    loop {
        if next_control(&mut link).await.ty == Durable::TYPE {
            break;
        }
    }
    let t0 = Instant::now();
    let done = job_done(&mut link, Duration::from_secs(15)).await;
    assert_eq!(done.status, gen::ERR_STALLED, "{:?}", done.message);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    srv.knob("progress_ms", 0);
    srv.knob("resume_progress_ms", 0);
}

/// A resumed job (its journal holds a done file) waits for the resume limit: silent past the fresh
/// limit it is still alive, and it is cut once the resume limit passes.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_job_gets_the_resume_limit() {
    let (srv, d, ids) = server("stall-resumed", 0);
    srv.knob("progress_ms", 800);
    srv.knob("resume_progress_ms", 4000);
    let job = [0x65; 16];
    {
        let (mut link, s) = opened(&srv, &d, ids.clone(), job, 3).await;
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"one!"))
            .await
            .unwrap();
        loop {
            if next_control(&mut link).await.ty == Durable::TYPE {
                break;
            }
        }
        drop(link);
        s.close().await;
    }
    let (mut link, _s) = opened(&srv, &d, ids.clone(), job, 3).await;
    let _lane = link.opener().unwrap().open().await.unwrap();
    let t0 = Instant::now();
    let done = job_done(&mut link, Duration::from_secs(20)).await;
    assert_eq!(done.status, gen::ERR_STALLED, "{:?}", done.message);
    assert!(
        t0.elapsed() >= Duration::from_millis(3500),
        "a resumed job outlives the fresh limit: {:?}",
        t0.elapsed()
    );
    srv.knob("progress_ms", 0);
    srv.knob("resume_progress_ms", 0);
}
