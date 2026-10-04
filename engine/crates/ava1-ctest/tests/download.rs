#![cfg(unix)]
//! Task 18: downloads, C sender (ava1_send.c) → Rust receiver (ava1::recv). The four
//! plan tests — a folder, a single ordered file, a resume from the engine's journal, a
//! refusal — plus the empty-folder test (ruling R3), and the Task 18 fix round: the C
//! sender's window rules (SPEC.md §12.3–§12.4) driven step by step, a transient send
//! failure, failed writer starts, and the wire Resume's re-attach.
mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ava1::gen::{self, JobCancel, JobMap, JobOpen, JobOpenAck, ManifestEnd, Resume};
use ava1::recv::{download_job, LocalSink, RecvOptions};
use ava1::router::{Inbound, JobLink};
use ava1::send::Progress;
use ava1::session::connect;
use ava1::wire::FrameMessage;
use ava1_ctest::{
    CSendWindow, CServer, C_E_CLOSED, C_E_IO, C_E_TOOLONG, SETTLE_EXIT, SETTLE_FATAL, SETTLE_GO_ON,
};
use common::*;

/// How long one download may take before the test fails it: a missing signal (a dead
/// job, a stalled sender) fails instead of hanging the round. Every real download here
/// finishes far below this.
const DL_DEADLINE: Duration = Duration::from_secs(300);

fn ro(jobs: &std::path::Path, ordered: bool) -> RecvOptions {
    RecvOptions {
        credit: 64 << 20,
        flags: 0, // `download_job` overwrites it with the flags argument
        jobs_dir: jobs.into(),
        ordered,
        progress: Arc::default(),
        cancel: Arc::default(),
        progress_deadline: None,
    }
}

async fn download(
    link: &mut ava1::router::JobLink,
    src: &str,
    flags: u32,
    sink: Arc<LocalSink>,
    o: RecvOptions,
) -> Result<ava1::recv::RecvReport, ava1::send::SendError> {
    tokio::time::timeout(DL_DEADLINE, download_job(link, src, flags, sink, o))
        .await
        .expect("the download did not finish within DL_DEADLINE")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_downloads_from_the_c_sender() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-folder");
    let src = d.join("console/game");
    write_tree(&src, 2000, |i| {
        if i % 400 == 0 {
            (6 << 20) + i
        } else {
            i % 3000
        }
    });
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x61; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = download(
        &mut link,
        src.to_str().unwrap(),
        0,
        sink,
        ro(&d.join("ejobs"), false),
    )
    .await
    .unwrap();
    assert_eq!(r.files, 2000);
    assert!(same_tree(&src, &d.join("got")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_file_downloads_in_order() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-single");
    let f = d.join("console/a.pkg");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(
        &f,
        (0..(40 << 20) + 9)
            .map(|i| (i * 5) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x62; 16]);
    let sink = Arc::new(LocalSink::new(d.join("a.pkg"), true));
    download(
        &mut link,
        f.to_str().unwrap(),
        gen::JF_ORDERED,
        sink,
        ro(&d.join("ejobs"), true),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(d.join("a.pkg")).unwrap(),
        std::fs::read(&f).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_resumes_from_the_engine_journal() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-resume");
    let f = d.join("console/big.bin");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(
        &f,
        (0..(128 << 20))
            .map(|i| (i * 13) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let progress = Arc::new(Progress::default());
    {
        let s = connect(&srv.addr(), me.clone(), mine.clone(), "rust", calm())
            .await
            .unwrap();
        let mut link = s.job([0x63; 16]);
        let sink = Arc::new(LocalSink::new(d.join("big.bin"), true));
        let mut o = ro(&d.join("ejobs"), false);
        o.progress = progress.clone();
        let pg = progress.clone();
        let cancel = o.cancel.clone();
        let poll = tokio::spawn(async move {
            let t = std::time::Instant::now();
            while pg.bytes_durable.load(Ordering::Relaxed) < 48 << 20 {
                assert!(
                    t.elapsed() < DL_DEADLINE,
                    "the first download never made 48 MiB durable"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(download(&mut link, f.to_str().unwrap(), 0, sink, o)
            .await
            .is_err());
        poll.await.unwrap();
    }
    let durable_before = progress.bytes_durable.load(Ordering::Relaxed);
    srv.knob("chunk_bytes", 0); // count what the C sender reads for the second run
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x63; 16]);
    let sink = Arc::new(LocalSink::new(d.join("big.bin"), true));
    let o2 = ro(&d.join("ejobs"), false);
    let pg2 = o2.progress.clone();
    download(&mut link, f.to_str().unwrap(), 0, sink, o2)
        .await
        .unwrap();
    assert!(pg2.bytes_durable.load(Ordering::Relaxed) >= 128 << 20);
    // The C sender's durable-range skip did skip: the first run was cancelled only once
    // at least 48 MiB were durable, so the resumed run sends at most the other 80 MiB
    // (plus each Chunk's few bytes of framing). Without this, a sender that ignored the
    // map's ranges and re-sent all 128 MiB would pass (only a false skip was pinned, by
    // the byte compare below).
    let sent = srv.sent_chunk_bytes();
    assert!(durable_before >= 48 << 20, "{durable_before}");
    assert!(sent > 0, "the resumed run sent nothing");
    assert!(
        sent <= (80 << 20) + (64 << 10),
        "the resumed run re-sent durable bytes: {sent} sent"
    );
    assert_eq!(
        std::fs::read(d.join("big.bin")).unwrap(),
        std::fs::read(&f).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_outside_the_allowed_roots_is_refused() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-refuse");
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    ava1_ctest::c_set_read_allowed(false);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x64; 16]);
    let sink = Arc::new(LocalSink::new(d.join("x"), false));
    let e = download(&mut link, "/etc", 0, sink, ro(&d.join("ejobs"), false))
        .await
        .unwrap_err();
    ava1_ctest::c_set_read_allowed(true);
    assert!(matches!(e, ava1::send::SendError::Refused { status, .. } if status == gen::ERR_PATH));
}

/// R3: an empty directory is a real tree shape — no manifest page, just ManifestEnd.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_folder_downloads() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-empty");
    let src = d.join("console/game");
    std::fs::create_dir_all(&src).unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x65; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = download(
        &mut link,
        src.to_str().unwrap(),
        0,
        sink,
        ro(&d.join("ejobs"), false),
    )
    .await
    .unwrap();
    assert_eq!(r.files, 0);
    assert!(d.join("got").exists());
}

// ---- the Task 18 fix round: the C sender's window, step by step ------------------------

/// Finding 1 (SPEC.md §12.3): a frame the writer handed to the socket may have reached
/// the receiver, so its lane's death must not return its charge — the sender would spend
/// the same window twice and the receiver's ERR_CREDIT would fail a healthy job. The
/// pre-fix sender refunded every in-flight frame of the dead lane.
#[test]
fn a_lane_death_keeps_the_charge_of_a_frame_the_writer_sent() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).expect("the frame fits");
    assert_eq!(w.settle(seq, 0), SETTLE_GO_ON);
    w.lane(1, false);
    let st = w.state();
    assert_eq!(st.credit, 40, "a lane death released a sent frame's charge");
    assert_eq!(
        (st.ready, st.inflight),
        (1, 0),
        "the frame is requeued for a re-send"
    );
    w.lane(2, true);
    assert_eq!(
        w.take(2),
        None,
        "the dead lane's bytes would be spent twice"
    );
    w.received(seq); // it had arrived after all: the receiver charged it...
    assert_eq!(w.state().credit, 40, "a late Received changes nothing here");
    w.credit(60); // ...and returns the bytes after its apply
    assert_eq!(w.state().credit, 100);
    assert!(w.take(2).is_some());
}

/// Finding 1, the frame mid-send: a lane that dies while its writer is inside the send
/// leaves that frame to the writer — requeued at once, another lane could send it and its
/// Received free the buffer the writer still reads. The writer settles it: a send that
/// completed keeps the charge (the frame may be on the wire)...
#[test]
fn a_frame_mid_send_when_its_lane_dies_is_the_writers_and_keeps_its_charge_if_sent() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).unwrap();
    w.lane(1, false); // the writer is inside ava1_server_send
    assert_eq!(w.state().ready, 0, "a frame still being sent was requeued");
    assert_eq!(w.settle(seq, 0), SETTLE_EXIT, "the writer's lane is gone");
    let st = w.state();
    assert_eq!(
        st.credit, 40,
        "a frame that left on the dead lane was released"
    );
    assert_eq!((st.ready, st.inflight), (1, 0));
}

/// ...and a send that failed never delivered a frame (a failed send writes nothing, or
/// breaks the connection mid-frame), so that charge provably comes back.
#[test]
fn a_frame_mid_send_when_its_lane_dies_is_released_if_the_send_failed() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).unwrap();
    w.lane(1, false);
    assert_eq!(w.state().ready, 0, "a frame still being sent was requeued");
    assert_eq!(w.settle(seq, C_E_CLOSED), SETTLE_EXIT);
    let st = w.state();
    assert_eq!(st.credit, 100, "a frame that never left kept its charge");
    assert_eq!((st.ready, st.inflight), (1, 0));
}

/// Finding 1: frames of many dead lanes, all confirmed late, leave the window exactly at
/// its grant. Pre-fix each lane death refunded its frames into a 256-slot ring that a
/// late Received re-charged from: past 256 refunds the ring was overwritten, the late
/// Received re-charged nothing, and the window overcounted (an over-send only the
/// receiver's ERR_CREDIT caught).
#[test]
fn many_dead_lanes_confirmed_late_leave_the_window_at_its_grant() {
    let w = CSendWindow::new(3000);
    let mut seqs = Vec::new();
    for round in 0..300u32 {
        let lane = (round % 8 + 1) as u16;
        w.lane(lane, true);
        assert_eq!(w.put(10), 0);
        let seq = w.take(lane).unwrap();
        assert_eq!(w.settle(seq, 0), SETTLE_GO_ON);
        w.lane(lane, false); // the frame may be on the wire: its charge is held
        seqs.push(seq);
    }
    assert_eq!(
        w.state().credit,
        0,
        "a dead lane's sent frames were released"
    );
    for seq in seqs {
        w.received(seq); // every one had arrived after all
    }
    w.credit(3000); // the receiver applied them all and returned the bytes
    assert_eq!(w.state().credit, 3000, "the window drifted from its grant");
}

/// Finding 2: a send error on a live lane (no lane-change follows a non-connection
/// error) must not kill the lane for good: the frame never left, so its charge comes back,
/// it is requeued, and the writer goes on. Pre-fix the writer exited with the frame
/// charged and in flight forever — a hang until the engine's deadline.
#[test]
fn a_transient_send_error_releases_the_frame_and_keeps_the_writer() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).unwrap();
    assert_eq!(
        w.settle(seq, C_E_IO),
        SETTLE_GO_ON,
        "the writer quit its live lane"
    );
    let st = w.state();
    assert_eq!(st.credit, 100, "a frame that never left kept its charge");
    assert_eq!(
        (st.ready, st.inflight),
        (1, 0),
        "the frame was not requeued"
    );
    let again = w.take(1).expect("the lane sends again");
    assert_ne!(again, seq, "a re-send takes a new seq");
    assert_eq!(w.settle(again, 0), SETTLE_GO_ON);
}

/// A frame the connection can never carry ends the job instead of retrying forever.
#[test]
fn a_frame_too_long_to_send_ends_the_job() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).unwrap();
    assert_eq!(w.settle(seq, C_E_TOOLONG), SETTLE_FATAL);
}

/// A Received that overtakes the writer's return from the send (the receiver is fast)
/// must not free the frame under the writer: the writer settles and frees it.
#[test]
fn a_received_during_the_send_leaves_the_frame_to_the_writer() {
    let w = CSendWindow::new(100);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    let seq = w.take(1).unwrap();
    w.received(seq);
    assert_eq!(w.state().inflight, 0, "the frame is accounted as received");
    assert_eq!(
        w.settle(seq, 0),
        SETTLE_GO_ON,
        "the writer's frame was freed under it"
    );
    let st = w.state();
    assert_eq!(
        (st.credit, st.ready),
        (40, 0),
        "the charge stays until the Credit"
    );
}

/// Minor 5: Credit.bytes is peer-controlled; a plain += wraps and shrinks the window.
#[test]
fn peer_credit_overflow_saturates_instead_of_wrapping() {
    let w = CSendWindow::new(u64::MAX / 2);
    w.credit(u64::MAX);
    assert_eq!(w.state().credit, u64::MAX);
}

/// Minor 6: a front frame larger than the credit no longer blocks a frame behind it
/// that fits (the head-of-line credit stall); the front stays queued.
#[test]
fn a_writer_skips_a_front_frame_that_does_not_fit_and_takes_one_behind_it() {
    let w = CSendWindow::new(50);
    w.lane(1, true);
    assert_eq!(w.put(60), 0);
    assert_eq!(w.put(40), 0);
    let seq = w
        .take(1)
        .expect("the fitting frame behind the front was not taken");
    let st = w.state();
    assert_eq!((st.credit, st.ready, st.queued), (10, 1, 60));
    assert_eq!(w.settle(seq, 0), SETTLE_GO_ON);
    assert_eq!(w.take(1), None, "nothing fits: nothing is taken");
}

/// Minor 7: once the job is ending the reader's frames go, never onto the queue — the
/// read-ahead bound held only until `stopping` was set.
#[test]
fn the_reader_stops_queueing_once_the_job_is_ending() {
    let w = CSendWindow::new(0);
    w.stopping();
    for _ in 0..4 {
        assert_ne!(
            w.put(16 << 20),
            0,
            "the put was accepted after the job ended"
        );
    }
    let st = w.state();
    assert_eq!((st.ready, st.queued), (0, 0));
}

// ---- the Task 18 fix round: end to end ------------------------------------------------

/// The deadline for the fix round's end-to-end downloads: each finishes in seconds; a
/// lane the sender abandoned hangs until this fires.
const FIX_DEADLINE: Duration = Duration::from_secs(60);

async fn fix_download(srv_knob: &[(&str, u32)], tag: &str, job: u8) {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir(tag);
    let f = d.join("console/f.bin");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(
        &f,
        (0..(24 << 20) + 5)
            .map(|i| (i * 11 + i / 977) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    for (k, v) in srv_knob {
        srv.knob(k, *v);
    }
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([job; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got.bin"), true));
    tokio::time::timeout(
        FIX_DEADLINE,
        download_job(
            &mut link,
            f.to_str().unwrap(),
            0,
            sink,
            ro(&d.join("ejobs"), false),
        ),
    )
    .await
    .expect("the download hung")
    .unwrap();
    assert_eq!(
        std::fs::read(d.join("got.bin")).unwrap(),
        std::fs::read(&f).unwrap()
    );
}

/// Finding 2 end to end: every lane's first send fails without writing (a transient
/// error that closes no connection, so no lane-change follows). Pre-fix each writer
/// exited with its frame charged and in flight forever, and the download hung.
#[tokio::test(flavor = "multi_thread")]
async fn a_download_survives_transient_send_failures() {
    fix_download(&[("send_fail", 8)], "dl-sendfail", 0x66).await;
}

/// Minor 8 end to end: writer starts that fail are retried on the next tick. Pre-fix
/// the lane was marked running with no writer, so with every start failing nothing was
/// ever sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_download_survives_writer_starts_that_fail() {
    fix_download(&[("writer_start_fail", 24)], "dl-wstartfail", 0x67).await;
}

async fn next_control_of(link: &mut JobLink, ty: u8) -> ava1::conn::Frame {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), link.rx.recv())
            .await
            .expect("no control frame within 10 s")
            .expect("the link closed")
        {
            Inbound::Control(f) if f.ty == ty => return f,
            _ => {}
        }
    }
}

/// Finding 3: a wire Resume re-attaches the sender job to a new session whose lanes came
/// up before the attach (so no lane-change ever reached the job). Pre-fix no writer
/// started for them and nothing was ever sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_wire_resume_starts_writers_for_the_lanes_already_up() {
    tokio::time::timeout(Duration::from_secs(90), async {
        ava1_ctest::c_set_read_allowed(true);
        let d = dir("dl-wire-resume");
        let f = d.join("console/f.bin");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, vec![0x5au8; 8 << 20]).unwrap();
        let (me, mine) = paired_client(&d.join("peers"));
        let srv = CServer::start_data(
            SECRET,
            &d.join("peers"),
            &d.join("jobs"),
            200,
            2000,
            2000,
            0,
        );
        let job = [0x68u8; 16];
        let hash;
        {
            // Session A opens the download and answers the map, with no lane: the
            // reader queues frames that nothing sends yet.
            let s = connect(&srv.addr(), me.clone(), mine.clone(), "rust", calm())
                .await
                .unwrap();
            let mut link = s.job(job);
            link.control
                .send(&JobOpen {
                    job_id: job,
                    kind: gen::JOB_DOWNLOAD,
                    root: f.to_str().unwrap().into(),
                    credit: Some(64 << 20),
                    ..Default::default()
                })
                .await
                .unwrap();
            let ack: JobOpenAck = next_control_of(&mut link, JobOpenAck::TYPE)
                .await
                .decode()
                .unwrap();
            assert_eq!(ack.status, 0);
            let end: ManifestEnd = next_control_of(&mut link, ManifestEnd::TYPE)
                .await
                .decode()
                .unwrap();
            hash = end.manifest_hash;
            link.control
                .send(&JobMap {
                    job_id: job,
                    status: 0,
                    last: 1,
                    ..Default::default()
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            s.close().await;
        }
        let t = std::time::Instant::now();
        while srv.job_attached(job) != 0 {
            assert!(t.elapsed() < Duration::from_secs(5), "never parked");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Session B: the lane first, then the Resume.
        let s = connect(&srv.addr(), me, mine, "rust", calm())
            .await
            .unwrap();
        let mut link = s.job(job);
        let lane = link.opener().unwrap().open().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await; // the lane-up reached no job
        link.control
            .send(&Resume {
                job_id: job,
                manifest_hash: hash,
            })
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match link.rx.recv().await {
                    Some(Inbound::Lane { lane: l, .. }) => return l,
                    Some(_) => {}
                    None => panic!("the link closed"),
                }
            }
        })
        .await
        .expect("no lane frame after the Resume: the lane's writer never started");
        assert_eq!(got, lane);
        let _ = link
            .control
            .send(&JobCancel {
                job_id: job,
                reason: 0,
            })
            .await;
    })
    .await
    .expect("the test hung");
}

/// Performance regression (T28): 2,000 tiny files downloaded from the C sender must not
/// crawl. On the console this ran at 140-300 files/s against FTX2's 2,150+; the root cause was the
/// receiver flushing the drive cache once per file (F_FULLFSYNC on macOS).
#[tokio::test(flavor = "multi_thread")]
async fn two_thousand_tiny_files_download_fast() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-perf");
    let src = d.join("console/tiny");
    write_tree(&src, 2000, |i| 1024 + (i * 977) % (63 * 1024));
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x63; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let t = std::time::Instant::now();
    let r = download(
        &mut link,
        src.to_str().unwrap(),
        0,
        sink,
        ro(&d.join("ejobs"), false),
    )
    .await
    .unwrap();
    let secs = t.elapsed().as_secs_f64();
    let rate = r.files as f64 / secs;
    eprintln!(
        "tiny download: {} files in {secs:.2}s = {rate:.0} files/s",
        r.files
    );
    assert_eq!(r.files, 2000);
    assert!(same_tree(&src, &d.join("got")));
    assert!(
        rate >= FLOOR_FILES_PER_S,
        "{rate:.0} files/s < {FLOOR_FILES_PER_S}"
    );
}

/// Measured on a Mac (loopback): the debug build (what `cargo test` runs; blake3 and the
/// AEAD are unoptimised) does ~1,400 files/s, `--release` ~4,500-5,000. The history this
/// floor guards: the per-file full-drive flush measured 56; after the batched sync, 430 debug
/// and ~2,500-3,000 release (serial per-file writes, a 250 ms wait before the final sync);
/// the parallel bundle writes and the immediate final sync took it to today's numbers. The
/// floors sit at ~70% of healthy, above everything the earlier pipeline reached. Raised from 700 to 1,000
/// (debug) with the pack log work (review 003 §3.2): 2,000 files measured 2,270-2,930 files/s on loopback, with
/// the log on or off, while this test ran alone; the whole binary's other tests run beside it.
/// Under the sanitizers (AVA1_CTEST_SANITIZE, review 009 #2a) the C runs 5-7x slower (measured 195
/// files/s): the floor there only catches a stall, not a regression of the pipeline.
const FLOOR_FILES_PER_S: f64 = if cfg!(ava1_ctest_sanitize) {
    50.0
} else if cfg!(debug_assertions) {
    1000.0
} else {
    3500.0
};
