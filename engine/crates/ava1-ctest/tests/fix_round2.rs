#![cfg(unix)]
//! Task 14 fix round 2: a cap refusal landing while a JobOpen's slow work runs refuses the
//! open all the same (the flag is re-checked after the work), and a full retiring table
//! refuses creates instead of silently losing the reopen guard. Every network test runs
//! under one outer timeout, so a hang fails in seconds.
mod common;

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use ava1::conn::Frame;
use ava1::gen::{self, JobOpen, JobOpenAck, ManifestEnd};
use ava1::manifest::{Entry, Manifest};
use ava1::router::{Inbound, JobLink};
use ava1::session::{connect, Session};
use ava1::wire::FrameMessage;
use ava1_ctest::CServer;
use common::*;

extern "C" {
    /// Tests only: the slow work behind a JobOpen waits this long, so a test can land a
    /// pipelined cap refusal inside the open's window (between the two refusal checks).
    fn ava1_test_set_open_work_delay_ms(ms: u32);
    /// 0 = the full retiring table refused a create and, drained, took one again.
    fn ava1_test_retiring_full_blocks_create() -> std::os::raw::c_int;
}

/// Both tests here drive the same C data layer: one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The whole body of a network test: anything that hangs fails the test here.
async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("the test hung (its outer timeout fired)")
}

/// The next control frame of type `ty`, skipping the rest.
async fn next_of(link: &mut JobLink, ty: u8) -> Frame {
    loop {
        let f = tokio::time::timeout(Duration::from_secs(15), link.rx.recv())
            .await
            .unwrap()
            .unwrap();
        match f {
            Inbound::Control(f) if f.ty == ty => return f,
            _ => {}
        }
    }
}

fn file(path: &str, size: u64) -> Entry {
    Entry {
        kind: gen::ENTRY_FILE,
        mode: 0o644,
        size,
        mtime: 1,
        path: path.into(),
        root: None,
    }
}

fn open_msg(job: [u8; 16], root: &Path) -> JobOpen {
    JobOpen {
        job_id: job,
        kind: gen::JOB_UPLOAD,
        root: root.to_str().unwrap().into(),
        ..Default::default()
    }
}

async fn open(link: &mut JobLink, job: [u8; 16], root: &Path) -> JobOpenAck {
    link.control.send(&open_msg(job, root)).await.unwrap();
    next_of(link, JobOpenAck::TYPE).await.decode().unwrap()
}

async fn send_manifest(link: &JobLink, pages: Vec<ava1::gen::ManifestPage>, end: ManifestEnd) {
    for p in &pages {
        link.control.send(p).await.unwrap();
    }
    link.control.send(&end).await.unwrap();
}

type Ids = (
    std::sync::Arc<ava1::keys::Identity>,
    std::sync::Arc<std::sync::Mutex<ava1::peers::PeerStore>>,
);

/// The C server reads its peers file once, at start: pair before starting it.
async fn session(srv: &CServer, ids: &Ids) -> Session {
    connect(&srv.addr(), ids.0.clone(), ids.1.clone(), "rust", calm())
        .await
        .unwrap()
}

/// Fix round 2, R2: a cap refusal that lands while a JobOpen's slow work runs (the
/// open-work-delay knob keeps the work in progress) must refuse the open all the same —
/// the flag is re-checked after the work. Pre-fix, the flag was consumed once, before the
/// work: the open proceeded, the sender's dropped manifest pages never reached the job,
/// and the sender waited for a JobMap until its own deadline.
#[tokio::test(flavor = "multi_thread")]
async fn a_cap_refusal_landing_mid_open_refuses_the_open_busy() {
    let _g = SERIAL.lock().await;
    bounded(async {
        let d = dir("fix2-midopen");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("ctl_cap", 65536);
        unsafe { ava1_test_set_open_work_delay_ms(1500) };
        let s = session(&srv, &ids).await;
        let job = [0x72u8; 16];
        // Build every frame BEFORE the open: under the sanitizers the debug-build manifest pages and
        // hash took over a second, which let the open's work finish before the pages left.
        let m = Manifest {
            entries: (0..5000).map(|i| file(&format!("f{i}"), 1)).collect(),
        };
        let pages = m.pages(job);
        let end = ManifestEnd {
            job_id: job,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        };
        let mut link = s.job(job);
        link.control
            .send(&open_msg(job, &d.join("dest")))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await; /* the open is mid-work now */
        send_manifest(&link, pages, end).await; /* past the cap, while the work runs */
        let ack: JobOpenAck = next_of(&mut link, JobOpenAck::TYPE).await.decode().unwrap();
        assert_eq!(ack.status, gen::ERR_BUSY);
        assert!(!s.is_closed());
        assert_eq!(srv.job_attached(job), -1); /* created mid-open, freed by the refusal */
        // The cap freed once the refused open's queue drained: a new open goes through.
        unsafe { ava1_test_set_open_work_delay_ms(0) };
        assert_eq!(open(&mut link, job, &d.join("dest2")).await.status, 0);
    })
    .await
}

/// Fix round 2, R4: with the retiring table full (every slot's job unlisted and still
/// being destroyed), a create must refuse — pre-fix it succeeded and its own unlist would
/// have silently lost the reopen guard.
#[tokio::test(flavor = "multi_thread")]
async fn a_full_retiring_table_refuses_creates() {
    let _g = SERIAL.lock().await;
    assert_eq!(unsafe { ava1_test_retiring_full_blocks_create() }, 0);
}
