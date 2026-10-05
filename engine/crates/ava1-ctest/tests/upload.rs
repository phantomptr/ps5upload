#![cfg(unix)]
//! Task 16: end-to-end uploads, Rust sender (Task 15) → C receiver (Task 13/14), under
//! faults — lane death, payload restart/resume, slow disk under a flood of tiny files,
//! and a source that changes while the payload is down.
mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ava1::send::Progress;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::*;

const MIB: u64 = 1 << 20;

/// Polls `f` every 10 ms until it holds. A timeout is a missing signal: the test fails
/// with the upload's counters (acked, durable and their gap) instead of hanging the round.
async fn wait_for(what: &str, ms: u64, pg: &Progress, mut f: impl FnMut() -> bool) {
    let t = std::time::Instant::now();
    while !f() {
        let sent = pg.bytes_sent.load(Ordering::Relaxed);
        let durable = pg.bytes_durable.load(Ordering::Relaxed);
        assert!(
            t.elapsed() < Duration::from_millis(ms),
            "timed out after {ms} ms waiting for {what}: sent={sent} durable={durable} \
             gap={} lanes={} resent={}",
            sent.saturating_sub(durable),
            pg.lanes.load(Ordering::Relaxed),
            pg.resent_bytes.load(Ordering::Relaxed),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_of_mixed_sizes_arrives_staged_and_whole() {
    let d = dir("e2e-folder");
    let src = d.join("src");
    write_tree(&src, 3000, |i| {
        if i % 500 == 0 {
            (5 * MIB) as usize + i
        } else {
            i % 4000
        }
    });
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let root = d.join("dest");
    let (r, sessions) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [1; 16],
        |_| {},
    )
    .await;
    assert_eq!((r.status, sessions), (0, 1));
    assert!(same_tree(&src, &root));
    assert!(!d.join("dest.ava-part").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_large_file_uses_several_lanes() {
    let d = dir("e2e-single");
    let f = d.join("big.bin");
    std::fs::write(
        &f,
        (0..(96 * MIB) as usize + 5)
            .map(|i| (i * 7) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let dest = d.join("out/big.bin");
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &f,
        dest.to_str().unwrap(),
        [2; 16],
        |_| {},
    )
    .await;
    assert_eq!(r.status, 0);
    assert!(r.max_lanes >= 2);
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&f).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn lane_killed_mid_chunk_requeues_and_completes() {
    let d = dir("e2e-lanekill");
    let f = d.join("big.bin");
    std::fs::write(
        &f,
        (0..(128 * MIB) as usize)
            .map(|i| (i * 3) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let px = Arc::new(
        ChaosProxy::start(
            srv.addr().parse().unwrap(),
            ChaosConfig {
                bytes_per_sec: Some(40 * MIB),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let progress = Arc::new(Progress::default());
    let (pg, pg2) = (progress.clone(), progress.clone());
    let px2 = px.clone();
    let killer = tokio::spawn(async move {
        // Up to 8 kills, stopping once a requeued frame was resent: a kill can land on a lane
        // with nothing in flight (the newest lane idle, as under the sanitizers), which proves
        // nothing either way.
        for _ in 0..8 {
            if pg2.resent_bytes.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
            // R2: only kill while a lane is open, so the kill lands on a lane, not on
            // control (which would cost the session).
            wait_for("a lane open before a kill", 5_000, &pg2, || {
                pg2.lanes.load(Ordering::Relaxed) > 0
            })
            .await;
            px2.kill_newest();
        }
    });
    let dest = d.join("out.bin");
    let (r, sessions) = upload(
        &px.addr.to_string(),
        me,
        mine,
        &f,
        dest.to_str().unwrap(),
        [3; 16],
        move |o| o.progress = pg.clone(),
    )
    .await;
    killer.await.unwrap();
    assert_eq!(r.status, 0);
    assert_eq!(
        sessions,
        1,
        "a lane kill cost the session: lanes={} sent={} durable={} resent={}",
        progress.lanes.load(Ordering::Relaxed),
        progress.bytes_sent.load(Ordering::Relaxed),
        progress.bytes_durable.load(Ordering::Relaxed),
        progress.resent_bytes.load(Ordering::Relaxed),
    );
    assert!(
        progress.resent_bytes.load(Ordering::Relaxed) > 0,
        "the dead lanes' frames were requeued"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&f).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_after_server_restart_resends_no_durable_byte() {
    let d = dir("e2e-restart");
    let f = d.join("big.bin");
    let size = 256 * MIB;
    std::fs::write(
        &f,
        (0..size as usize)
            .map(|i| (i * 11) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let mut srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let addr = srv.addr();
    let progress = Arc::new(Progress::default());
    let (pg, pg2) = (progress.clone(), progress.clone());
    let dest = d.join("out.bin");
    let dest_s = dest.to_str().unwrap().to_string();
    let up = tokio::spawn(async move {
        upload(&addr, me, mine, &f, &dest_s, [4; 16], move |o| {
            o.progress = pg.clone()
        })
        .await
    });
    // The kill must land behind the durable mark: some bytes have to be acked but not yet
    // durable when the payload dies, or Review Focus 3's bound has nothing to pin. In
    // Docker the sender paces with the receiver's apply, so that gap is transient (it
    // oscillates with the chunk in flight); waiting for it makes the state the test
    // examines deterministic. Expiry is a failure with the counters, never a skip.
    let mut kill = None;
    wait_for(
        "half the file durable with acked-not-durable bytes in flight",
        30_000,
        &pg2,
        || {
            let sent = pg2.bytes_sent.load(Ordering::Relaxed);
            let durable = pg2.bytes_durable.load(Ordering::Relaxed);
            if durable >= size / 2 && sent.saturating_sub(durable) > 0 {
                kill = Some((sent, durable));
            }
            kill.is_some()
        },
    )
    .await;
    let (sent_at_kill, durable_at_kill) = kill.unwrap();
    let acked_not_durable = sent_at_kill.saturating_sub(durable_at_kill);
    // R1's non-vacuity guards: the kill really landed behind the durable mark (some
    // bytes were acked but not yet durable) and ahead of the end of the file.
    assert!(
        acked_not_durable > 0,
        "acked ({sent_at_kill}) met durable ({durable_at_kill}) at the kill"
    );
    assert!(
        acked_not_durable < size / 2,
        "acked-not-durable at the kill: {acked_not_durable}"
    );
    srv.restart_data(); // the payload dies and comes back; memory gone, journal kept
    let (r, sessions) = up.await.unwrap();
    assert_eq!(r.status, 0);
    assert!(sessions >= 2);
    let resent = progress.resent_bytes.load(Ordering::Relaxed);
    // R1 (kept as an additional check): `resent_bytes` counts only frames whose `resend`
    // flag was set, and that flag is set only when a dead lane's frames are requeued. The
    // resumed session reads its frames fresh from the source (resend=false), so this is ~0
    // after a payload restart no matter what session 2 sends — it cannot carry the claim
    // on its own. The honest oracle is R2 below.
    assert!(
        resent <= acked_not_durable + 8 * MIB,
        "resent {resent}, acked-not-durable at kill {acked_not_durable}"
    );
    assert!(durable_at_kill >= size / 2);
    // R2: the honest oracle for session 2. `bytes_sent` counts acked payload bytes, so
    // `acked_end - acked_at_kill` is exactly what the resumed session delivered. The job
    // may legitimately still owe everything that was not durable at the kill (the sender
    // resumes at the durable frontier and reads the rest fresh), plus 8 MiB of slack for
    // the frame at the frontier and the snapshot race — nothing more. A sender that
    // ignored the journal and re-sent every durable byte would ack ~size in session 2;
    // the bound is strictly below size (durable_at_kill >= size/2), so that re-send fails
    // it.
    let acked_end = progress.bytes_sent.load(Ordering::Relaxed);
    let session2_acked = acked_end - sent_at_kill;
    let owed = size - durable_at_kill;
    let bound = owed + 8 * MIB;
    assert!(
        bound < size,
        "R2 bound {bound} must stay strictly below the file size {size}, or a full re-send \
         would pass it (durable at kill {durable_at_kill})"
    );
    assert!(
        session2_acked <= bound,
        "session 2 acked {session2_acked} > owed {owed} + 8 MiB slack: the sender re-sent \
         durable bytes (acked at kill {sent_at_kill}, acked at end {acked_end})"
    );
    assert_eq!(std::fs::read(&dest).unwrap().len() as u64, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn tiny_flood_on_a_slow_disk_keeps_the_session_alive() {
    let d = dir("e2e-flood");
    let src = d.join("src");
    write_tree(&src, 20_000, |_| 1024);
    // Every fsync takes 2 ms: apply is far slower than the network.
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 2000);
    let root = d.join("dest");
    let (r, sessions) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [5; 16],
        |_| {},
    )
    .await;
    assert_eq!(r.status, 0);
    assert_eq!(sessions, 1, "liveness never failed while apply was slow");
    assert!(same_tree(&src, &root));
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_source_restarts_the_file_never_splices() {
    let d = dir("e2e-changed");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let f = src.join("v.bin");
    std::fs::write(&f, vec![0xAA; (64 * MIB) as usize]).unwrap();
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let mut srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let addr = srv.addr();
    let progress = Arc::new(Progress::default());
    let (pg, pg2) = (progress.clone(), progress.clone());
    let root = d.join("dest");
    let (src2, root_s) = (src.clone(), root.to_str().unwrap().to_string());
    let (me2, mine2) = (me.clone(), mine.clone());
    let up = tokio::spawn(async move {
        upload(&addr, me2, mine2, &src2, &root_s, [6; 16], move |o| {
            o.progress = pg.clone()
        })
        .await
    });
    wait_for("16 MiB durable", 30_000, &pg2, || {
        pg2.bytes_durable.load(Ordering::Relaxed) >= 16 * MIB
    })
    .await;
    // The file changes (new bytes, new mtime) while the payload is down.
    srv.stop_data_only();
    std::fs::write(&f, vec![0xBB; (64 * MIB) as usize]).unwrap();
    let t = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(t)
        .unwrap();
    srv.start_data_again();
    let (r, _) = up.await.unwrap();
    assert_eq!(r.status, 0);
    let out = std::fs::read(root.join("v.bin")).unwrap();
    assert!(
        out.iter().all(|b| *b == 0xBB),
        "old and new bytes were spliced"
    );
}
