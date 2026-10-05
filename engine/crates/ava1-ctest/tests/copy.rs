#![cfg(unix)]
mod common;

use std::time::Duration;

use ava1::gen::{self, JobCopy, JobRef, Status};
use ava1::session::{connect, Session};
use ava1::wire::Message;
use ava1_ctest::CServer;
use common::*;

// The C server is a process-wide singleton: run this binary with `--test-threads=1`.
// Every `start_data` blocks on the same lock until the previous test's server drops.

async fn status(s: &Session, job: [u8; 16]) -> Status {
    let r = s
        .rpc(
            gen::METHOD_JOB_STATUS,
            &JobRef { job_id: job }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    Status::decode(&r.body).unwrap()
}

async fn wait_finished(s: &Session, job: [u8; 16]) -> Status {
    for _ in 0..1200 {
        let st = status(s, job).await;
        if st.state.unwrap_or(0) != 0 {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("copy did not finish");
}

async fn start(
    s: &Session,
    job: [u8; 16],
    src: &std::path::Path,
    dest: &std::path::Path,
    flags: u32,
) -> u16 {
    let body = JobCopy {
        job_id: job,
        src: src.to_str().unwrap().into(),
        dest: dest.to_str().unwrap().into(),
        flags,
    };
    s.rpc(gen::METHOD_JOB_COPY, &body.to_bytes().unwrap())
        .await
        .unwrap()
        .status
}

/// A move's delete follows the journaled Done by a moment (the journal fsync sits between
/// the terminal state and the unlinks): poll, bound so a missing delete fails the test
/// instead of hanging it.
async fn wait_gone(p: &std::path::Path) {
    for _ in 0..200 {
        if !p.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{} was never deleted", p.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn another_device_cannot_inspect_cancel_or_reopen_a_copy() {
    let d = dir("copy-owner");
    write_tree(&d.join("usb/g"), 100, |_| 4096);
    let (owner, owner_peers) = paired_client(&d.join("peers"));
    let (other, other_peers) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let a = connect(&srv.addr(), owner, owner_peers, "owner", calm())
        .await
        .unwrap();
    let b = connect(&srv.addr(), other, other_peers, "other", calm())
        .await
        .unwrap();
    let id = [0x91; 16];
    let src = d.join("usb/g");
    let dest = d.join("data/g");
    assert_eq!(start(&a, id, &src, &dest, 0).await, gen::STATUS_OK);
    let reference = JobRef { job_id: id }.to_bytes().unwrap();
    for method in [gen::METHOD_JOB_STATUS, gen::METHOD_JOB_CANCEL] {
        assert_eq!(
            b.rpc(method, &reference).await.unwrap().status,
            gen::ERR_UNKNOWN_JOB
        );
    }
    assert_eq!(start(&b, id, &src, &dest, 0).await, gen::ERR_UNKNOWN_JOB);
    assert_eq!(
        a.rpc(gen::METHOD_JOB_STATUS, &reference)
            .await
            .unwrap()
            .status,
        gen::STATUS_OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_source_walk_does_not_block_another_open() {
    let d = dir("copy-slow-walk");
    write_tree(&d.join("usb/large"), 100, |_| 1024);
    std::fs::write(d.join("small.bin"), b"small").unwrap();
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
    let s = std::sync::Arc::new(
        connect(&srv.addr(), me, mine, "rust", calm())
            .await
            .unwrap(),
    );
    srv.knob("copy_walk_delay_ms", 1500);
    let first_session = s.clone();
    let first_src = d.join("usb/large");
    let first_dest = d.join("data/large");
    let first =
        tokio::spawn(
            async move { start(&first_session, [0x92; 16], &first_src, &first_dest, 0).await },
        );
    for _ in 0..100 {
        if srv.copy_walk_active() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(srv.copy_walk_active(), "the source walk did not begin");
    srv.knob("copy_walk_delay_ms", 0);
    let second = tokio::time::timeout(
        Duration::from_millis(700),
        start(
            &s,
            [0x93; 16],
            &d.join("small.bin"),
            &d.join("data/small.bin"),
            0,
        ),
    )
    .await
    .expect("another open waited for the first source walk");
    assert_eq!(second, gen::STATUS_OK);
    assert_eq!(first.await.unwrap(), gen::STATUS_OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn reaper_keeps_a_running_copy_and_ages_a_finished_one() {
    let d = dir("copy-reap");
    write_tree(&d.join("usb/g"), 3000, |_| 1024);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    srv.knob("park_ms", 250);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let id = [0x94; 16];
    assert_eq!(
        start(&s, id, &d.join("usb/g"), &d.join("data/g"), 0).await,
        0
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        status(&s, id).await.state,
        Some(0),
        "a running copy was reaped or finished too soon"
    );
    assert_eq!(wait_finished(&s, id).await.state, Some(1));
    let reference = JobRef { job_id: id }.to_bytes().unwrap();
    for _ in 0..40 {
        let reply = s.rpc(gen::METHOD_JOB_STATUS, &reference).await.unwrap();
        if reply.status == gen::ERR_UNKNOWN_JOB {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("a finished copy stayed listed past its park age");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_keeps_a_source_file_changed_after_copying() {
    let d = dir("copy-source-changed");
    let src = d.join("usb/g");
    write_tree(&src, 2, |_| 4096);
    let changed = src.join("d00/f00000");
    let before = std::fs::read(&changed).unwrap();
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
    srv.knob("copy_delete_delay_ms", 1000);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let id = [0x95; 16];
    assert_eq!(
        start(&s, id, &src, &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    for _ in 0..200 {
        if srv.copy_delete_active() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        srv.copy_delete_active(),
        "the move never began deleting its source"
    );
    std::fs::write(&changed, vec![0x55; before.len() + 1]).unwrap();
    let st = wait_finished(&s, id).await;
    assert_eq!(
        st.state,
        Some(2),
        "a changed source file was silently deleted"
    );
    assert_eq!(
        std::fs::read(&changed).unwrap(),
        vec![0x55; before.len() + 1]
    );
    assert_eq!(std::fs::read(d.join("data/g/d00/f00000")).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_copies_on_the_console() {
    let d = dir("copy");
    write_tree(&d.join("usb/game"), 1500, |i| {
        if i % 300 == 0 {
            (3 << 20) + i
        } else {
            i % 2000
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
    let job = [0x71; 16];
    assert_eq!(
        start(&s, job, &d.join("usb/game"), &d.join("data/game"), 0).await,
        gen::STATUS_OK
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert_eq!(st.files_done, 1500);
    assert_eq!(st.files_total, 1500);
    assert!(same_tree(&d.join("usb/game"), &d.join("data/game")));
    // A finished copy stays listed so status keeps answering (10-minute window).
    assert_eq!(status(&s, job).await.state, Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_deletes_the_source_only_after_a_verified_copy() {
    let d = dir("move");
    write_tree(&d.join("usb/g"), 300, |i| i * 11);
    let keep = d.join("keep");
    copy_dir(&d.join("usb/g"), &keep);
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
    let job = [0x72; 16];
    assert_eq!(
        start(&s, job, &d.join("usb/g"), &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    assert_eq!(wait_finished(&s, job).await.state, Some(1));
    wait_gone(&d.join("usb/g")).await;
    assert!(same_tree(&keep, &d.join("data/g")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_file_copies_and_moves() {
    let d = dir("copy-file");
    std::fs::create_dir_all(d.join("usb")).unwrap();
    std::fs::write(d.join("usb/one.bin"), vec![7u8; 700_000]).unwrap();
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
    let job = [0x75; 16];
    assert_eq!(
        start(
            &s,
            job,
            &d.join("usb/one.bin"),
            &d.join("data/one.bin"),
            gen::JF_MOVE
        )
        .await,
        0
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert_eq!(st.files_done, 1);
    wait_gone(&d.join("usb/one.bin")).await;
    assert_eq!(
        std::fs::read(d.join("data/one.bin")).unwrap(),
        vec![7u8; 700_000]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_folder_copies_on_the_console() {
    let d = dir("copy-empty");
    std::fs::create_dir_all(d.join("usb/empty")).unwrap();
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
    let job = [0x76; 16];
    assert_eq!(
        start(&s, job, &d.join("usb/empty"), &d.join("data/empty"), 0).await,
        0
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert_eq!(st.files_total, 0);
    assert!(d.join("data/empty").is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_into_its_own_source_is_refused() {
    let d = dir("copy-self");
    std::fs::create_dir_all(d.join("a")).unwrap();
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
    assert_eq!(
        start(&s, [0x73; 16], &d.join("a"), &d.join("a/b"), 0).await,
        gen::ERR_PATH
    );
    assert_eq!(
        start(&s, [0x73; 16], &d.join("a"), &d.join("a"), 0).await,
        gen::ERR_PATH
    );
    // The other direction too: the written namespace would reach into the read tree.
    std::fs::create_dir_all(d.join("a/b")).unwrap();
    assert_eq!(
        start(&s, [0x7a; 16], &d.join("a/b"), &d.join("a"), 0).await,
        gen::ERR_PATH
    );
    // A destination that simply shares a prefix is not "inside": /ab is not under /a. The
    // flag is JF_OVERWRITE because the folder `ab` exists (C14 refuses it otherwise).
    std::fs::create_dir_all(d.join("ab")).unwrap();
    assert_eq!(
        start(
            &s,
            [0x7b; 16],
            &d.join("a/b"),
            &d.join("ab"),
            gen::JF_OVERWRITE
        )
        .await,
        0
    );
    assert_eq!(wait_finished(&s, [0x7b; 16]).await.state, Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_refuses_symlink_and_dotdot_aliases_of_its_source() {
    use std::os::unix::fs::symlink;

    let d = dir("copy-move-alias");
    let src = d.join("usb/g");
    std::fs::create_dir_all(src.join("d00")).unwrap();
    std::fs::write(src.join("keep.bin"), b"only copy").unwrap();
    symlink(d.join("usb"), d.join("alias")).unwrap();
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
    for (id, from, to) in [
        (0x81, d.join("usb/g/"), d.join("usb/g/b")),
        (0x82, d.join("usb/g/d00/.."), d.join("usb/g/c")),
        (0x83, d.join("usb/g"), d.join("alias/g")),
    ] {
        assert_eq!(
            start(&s, [id; 16], &from, &to, gen::JF_MOVE | gen::JF_OVERWRITE).await,
            gen::ERR_PATH,
            "unsafe move {from:?} -> {to:?} was accepted"
        );
        assert_eq!(std::fs::read(src.join("keep.bin")).unwrap(), b"only copy");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parent_sync_failure_keeps_the_moves_source() {
    let d = dir("copy-move-sync-fail");
    let src = d.join("usb/g");
    write_tree(&src, 6, |_| 4096);
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
    srv.fail_dir_sync(u32::MAX);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let job = [0x84; 16];
    assert_eq!(
        start(&s, job, &src, &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    let _ = wait_finished(&s, job).await;
    assert!(
        src.join("d00/f00000").exists(),
        "the source was deleted after a failed parent sync"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_copy_to_the_same_destination_is_refused() {
    let d = dir("copy-busy");
    write_tree(&d.join("usb/g"), 3000, |_| 2048);
    // 3 ms per fsync keeps the first copy running while the second one asks.
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let a = [0x77; 16];
    assert_eq!(
        start(&s, a, &d.join("usb/g"), &d.join("data/g"), 0).await,
        0
    );
    assert_eq!(
        start(&s, a, &d.join("usb/g"), &d.join("data/other"), 0).await,
        gen::ERR_PROTOCOL,
        "a reused id cannot change its destination"
    );
    assert_eq!(
        start(
            &s,
            [0x89; 16],
            &d.join("usb/g"),
            &d.join("data/other"),
            1 << 30
        )
        .await,
        gen::ERR_PROTOCOL,
        "unknown copy flags must be refused"
    );
    assert!(
        status(&s, a).await.files_done < 3000,
        "the first copy is still running"
    );
    assert_eq!(
        start(&s, [0x78; 16], &d.join("usb/g"), &d.join("data/g"), 0).await,
        gen::ERR_BUSY
    );
    assert_eq!(
        start(&s, [0x85; 16], &d.join("usb/g"), &d.join("data/g/child"), 0).await,
        gen::ERR_BUSY,
        "a nested writer must wait for the parent job"
    );
    // The same job id is not a second writer: it resumes.
    assert_eq!(
        start(&s, a, &d.join("usb/g"), &d.join("data/g"), 0).await,
        0
    );
    let c = s
        .rpc(
            gen::METHOD_JOB_CANCEL,
            &JobRef { job_id: a }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(c.status, gen::STATUS_OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_move_locks_its_source_against_writers() {
    let d = dir("copy-move-source-busy");
    write_tree(&d.join("usb/g"), 3000, |_| 2048);
    std::fs::create_dir_all(d.join("other")).unwrap();
    std::fs::write(d.join("other/one"), b"one").unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let moving = [0x86; 16];
    assert_eq!(
        start(
            &s,
            moving,
            &d.join("usb/g"),
            &d.join("data/g"),
            gen::JF_MOVE
        )
        .await,
        0
    );
    assert!(status(&s, moving).await.files_done < 3000);
    assert_eq!(
        start(
            &s,
            [0x87; 16],
            &d.join("other/one"),
            &d.join("usb/g/other"),
            gen::JF_OVERWRITE
        )
        .await,
        gen::ERR_BUSY,
        "a writer must not change a running move's source"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_reports_a_source_file_it_could_not_delete() {
    let d = dir("copy-move-delete-fail");
    let src = d.join("usb/g");
    write_tree(&src, 40, |_| 4096);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    srv.knob("copy_delete_delay_ms", 1000);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let job = [0x88; 16];
    assert_eq!(
        start(&s, job, &src, &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    for _ in 0..200 {
        if srv.copy_delete_active() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(srv.copy_delete_active());
    let extra = src.join("added-after-copy.bin");
    std::fs::write(&extra, b"must stay at source").unwrap();
    let st = wait_finished(&s, job).await;
    assert_eq!(
        st.state,
        Some(2),
        "a move with leftover source files reported success"
    );
    assert_eq!(std::fs::read(extra).unwrap(), b"must stay at source");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_copy_can_retry_under_the_same_id() {
    let d = dir("copy-retry-failed");
    let src = d.join("usb/g");
    write_tree(&src, 8, |_| 4096);
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
    srv.fail_dir_sync(u32::MAX);
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let job = [0x8a; 16];
    let dest = d.join("data/g");
    assert_eq!(start(&s, job, &src, &dest, 0).await, 0);
    assert_eq!(wait_finished(&s, job).await.state, Some(2));
    srv.fail_dir_sync(u32::MAX - 1);
    assert_eq!(start(&s, job, &src, &dest, 0).await, 0);
    let retry = wait_finished(&s, job).await;
    assert_eq!(retry.state, Some(1), "retry ended: {:?}", retry.current);
    assert!(same_tree(&src, &dest));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dangling_symlink_fails_the_copy() {
    let d = dir("copy-link");
    write_tree(&d.join("usb/g"), 4, |_| 64);
    std::os::unix::fs::symlink(d.join("usb/gone"), d.join("usb/g/d00/link")).unwrap();
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
    // Rust's walk fails the whole tree on a dangling symlink (source.rs): the C walk must
    // not silently drop the entry and copy a tree that is missing a file.
    assert_eq!(
        start(&s, [0x79; 16], &d.join("usb/g"), &d.join("data/g"), 0).await,
        gen::ERR_IO
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_resumes_after_a_payload_restart() {
    let d = dir("copy-resume");
    write_tree(&d.join("usb/g"), 3000, |_| 2048);
    let (me, mine) = paired_client(&d.join("peers"));
    let mut srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let job = [0x74; 16];
    {
        let s = connect(&srv.addr(), me.clone(), mine.clone(), "rust", calm())
            .await
            .unwrap();
        assert_eq!(
            start(&s, job, &d.join("usb/g"), &d.join("data/g"), 0).await,
            0
        );
        for _ in 0..1500 {
            // 30 s bound: the first copy must reach 500 durable files.
            if status(&s, job).await.files_done >= 500 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            status(&s, job).await.files_done >= 500,
            "the first copy made no progress"
        );
    }
    srv.restart_data();
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    assert_eq!(
        start(&s, job, &d.join("usb/g"), &d.join("data/g"), 0).await,
        0
    );
    let first = status(&s, job).await;
    assert!(
        first.files_done >= 500,
        "the journal's progress survived the restart"
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert!(same_tree(&d.join("usb/g"), &d.join("data/g")));
}

/* ---- fix round 2 ------------------------------------------------------------------- */

#[tokio::test(flavor = "multi_thread")]
async fn a_move_reissued_after_a_crash_following_done_deletes_its_source() {
    let d = dir("copy-move-crash-after-done");
    let src = d.join("usb/g");
    write_tree(&src, 6, |_| 4096);
    let (me, mine) = paired_client(&d.join("peers"));
    let mut srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let job = [0x8a; 16];
    {
        let s = connect(&srv.addr(), me.clone(), mine.clone(), "rust", calm())
            .await
            .unwrap();
        // The payload "dies" after the journaled Done(OK), before the delete phase.
        srv.knob("copy_crash_before_delete", 1);
        assert_eq!(
            start(&s, job, &src, &d.join("data/g"), gen::JF_MOVE).await,
            0
        );
        for _ in 0..400 {
            if srv.copy_delete_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(srv.copy_delete_active(), "the copy never reached its Done");
        assert!(src.join("d00/f00000").exists());
    }
    srv.restart_data();
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    assert_eq!(
        start(&s, job, &src, &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    wait_gone(&src).await;
    assert!(d.join("data/g/d00/f00000").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_out_of_a_tree_a_copy_is_still_writing_is_busy() {
    let d = dir("copy-move-src-in-writer");
    write_tree(&d.join("usb/g"), 3000, |_| 2048);
    std::fs::create_dir_all(d.join("w/g/sub")).unwrap();
    std::fs::write(d.join("w/g/sub/one"), b"one").unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let writer = [0x8b; 16];
    assert_eq!(
        start(
            &s,
            writer,
            &d.join("usb/g"),
            &d.join("w/g"),
            gen::JF_OVERWRITE
        )
        .await,
        0
    );
    assert!(status(&s, writer).await.files_done < 3000);
    assert_eq!(
        start(
            &s,
            [0x8c; 16],
            &d.join("w/g/sub"),
            &d.join("out/x"),
            gen::JF_MOVE
        )
        .await,
        gen::ERR_BUSY,
        "a move took its source out of a tree a copy is writing"
    );
    assert!(d.join("w/g/sub/one").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_through_an_alias_of_a_running_moves_source_is_busy() {
    let d = dir("copy-move-src-alias-busy");
    write_tree(&d.join("usb/g"), 3000, |_| 2048);
    std::os::unix::fs::symlink(d.join("usb/g"), d.join("alias")).unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let first = [0x8d; 16];
    assert_eq!(
        start(&s, first, &d.join("usb/g"), &d.join("data/g"), gen::JF_MOVE).await,
        0
    );
    assert!(status(&s, first).await.files_done < 3000);
    assert_eq!(
        start(
            &s,
            [0x8e; 16],
            &d.join("alias"),
            &d.join("data/h"),
            gen::JF_MOVE
        )
        .await,
        gen::ERR_BUSY,
        "two moves share one source through an alias"
    );
}

#[test]
fn a_chunk_or_bundle_that_fails_to_decode_returns_its_reserved_bytes() {
    assert_eq!(ava1_ctest::c_copy_put_decode_failure(), 0);
}

#[test]
fn a_changed_retry_reaches_the_job_with_the_source_changed_message() {
    assert_eq!(ava1_ctest::c_copy_retry_changed_message(), 0);
}

/* ---- C14: JF_OVERWRITE ------------------------------------------------------------- */

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_destination_is_refused_without_overwrite() {
    let d = dir("copy-exists");
    write_tree(&d.join("usb/g"), 12, |i| 100 + i);
    write_tree(&d.join("data/g"), 5, |i| 300 + i);
    let keep = d.join("keep");
    copy_dir(&d.join("data/g"), &keep);
    std::fs::create_dir_all(d.join("usb")).unwrap();
    std::fs::write(d.join("usb/one.bin"), b"source bytes").unwrap();
    std::fs::create_dir_all(d.join("data")).unwrap();
    std::fs::write(d.join("data/one.bin"), b"already here").unwrap();
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
    // A tree destination that already holds files: refused, and left byte-for-byte alone.
    assert_eq!(
        start(&s, [0x81; 16], &d.join("usb/g"), &d.join("data/g"), 0).await,
        gen::ERR_EXISTS
    );
    assert!(
        same_tree(&keep, &d.join("data/g")),
        "the destination was not left untouched"
    );
    // A single file whose destination exists: the same refusal.
    assert_eq!(
        start(
            &s,
            [0x82; 16],
            &d.join("usb/one.bin"),
            &d.join("data/one.bin"),
            0
        )
        .await,
        gen::ERR_EXISTS
    );
    assert_eq!(
        std::fs::read(d.join("data/one.bin")).unwrap(),
        b"already here"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overwrite_replaces_colliding_files_and_keeps_destination_only_files() {
    let d = dir("copy-overwrite");
    write_tree(&d.join("usb/g"), 8, |i| 1000 + i * 3);
    // The same layout at the destination: every file collides (different bytes), plus one
    // destination-only file that must survive.
    for i in 0..8 {
        let p = d.join("data/g").join(format!("d{:02}/f{i:05}", i % 37));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        if i == 3 {
            std::fs::write(&p, vec![0xEEu8; 200]).unwrap();
        } else {
            std::fs::write(&p, vec![0x11u8; 40]).unwrap();
        }
    }
    let extra = d.join("data/g/d00/extra");
    std::fs::write(&extra, b"dest only").unwrap();
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
    let job = [0x83; 16];
    assert_eq!(
        start(
            &s,
            job,
            &d.join("usb/g"),
            &d.join("data/g"),
            gen::JF_OVERWRITE
        )
        .await,
        0
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert_eq!(st.files_done, 8);
    for i in 0..8 {
        let rel = format!("d{:02}/f{i:05}", i % 37);
        assert_eq!(
            std::fs::read(d.join("data/g").join(&rel)).unwrap(),
            std::fs::read(d.join("usb/g").join(&rel)).unwrap(),
            "{rel} was not replaced with the source bytes"
        );
    }
    assert_eq!(
        std::fs::read(&extra).unwrap(),
        b"dest only",
        "a destination-only file must survive"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_file_with_overwrite_replaces_the_file() {
    let d = dir("copy-file-overwrite");
    std::fs::create_dir_all(d.join("usb")).unwrap();
    std::fs::write(d.join("usb/one.bin"), vec![7u8; 700_000]).unwrap();
    std::fs::create_dir_all(d.join("data")).unwrap();
    std::fs::write(d.join("data/one.bin"), vec![9u8; 123]).unwrap();
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
    let job = [0x84; 16];
    assert_eq!(
        start(
            &s,
            job,
            &d.join("usb/one.bin"),
            &d.join("data/one.bin"),
            gen::JF_SINGLE_FILE | gen::JF_OVERWRITE
        )
        .await,
        0
    );
    let st = wait_finished(&s, job).await;
    assert_eq!(st.state, Some(1), "{:?}", st.current);
    assert_eq!(st.files_done, 1);
    assert_eq!(
        std::fs::read(d.join("data/one.bin")).unwrap(),
        vec![7u8; 700_000]
    );
    assert!(d.join("usb/one.bin").exists(), "an overwrite is not a move");
}

/* ---- final review #9: a cancelled overwrite copy removes its own part files ------------ */

fn part_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().ends_with(".ava-part") {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_overwrite_copy_leaves_no_part_file_and_no_user_file_lost() {
    let d = dir("copy-cancel-parts");
    // Large files (above the cutoff, so each is written to its own `.ava-part`), sparse so the
    // test does not write gigabytes of source.
    for i in 0..6 {
        let p = d.join("usb/g").join(format!("big{i}"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::File::create(&p)
            .unwrap()
            .set_len(256 * 1024 * 1024)
            .unwrap();
    }
    // The destination exists (so the copy writes in place, not staged) with its own files: one
    // collides with a source file, one is the user's, and one merely looks like a part file.
    std::fs::create_dir_all(d.join("data/g")).unwrap();
    for i in 0..6 {
        std::fs::write(d.join("data/g").join(format!("big{i}")), b"original").unwrap();
    }
    std::fs::write(d.join("data/g/mine"), b"mine").unwrap();
    std::fs::write(d.join("data/g/notours.ava-part"), b"user file").unwrap();
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        3000,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let job = [0x9c; 16];
    assert_eq!(
        start(
            &s,
            job,
            &d.join("usb/g"),
            &d.join("data/g"),
            gen::JF_OVERWRITE
        )
        .await,
        0
    );
    let mut seen = false;
    for _ in 0..4000 {
        if part_files(&d.join("data/g"))
            .iter()
            .any(|p| p.file_name().unwrap() != "notours.ava-part")
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(seen, "the copy never began a part file (or finished first)");
    let r = s
        .rpc(
            gen::METHOD_JOB_CANCEL,
            &JobRef { job_id: job }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    // The console's terminal answer: it no longer knows the job.
    for _ in 0..200 {
        let r = s
            .rpc(
                gen::METHOD_JOB_STATUS,
                &JobRef { job_id: job }.to_bytes().unwrap(),
            )
            .await
            .unwrap();
        if r.status == gen::ERR_UNKNOWN_JOB {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Part files go once the job is destroyed (its threads joined): bounded wait.
    for _ in 0..200 {
        if part_files(&d.join("data/g")).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let left = part_files(&d.join("data/g"));
    assert_eq!(left, [d.join("data/g/notours.ava-part")], "{left:?}");
    assert_eq!(
        std::fs::read(d.join("data/g/notours.ava-part")).unwrap(),
        b"user file"
    );
    assert_eq!(std::fs::read(d.join("data/g/mine")).unwrap(), b"mine");
    for i in 0..6 {
        // Each is the user's original or (if its commit had landed) the finished source file:
        // never a partial one.
        let m = std::fs::metadata(d.join("data/g").join(format!("big{i}"))).unwrap();
        assert!(
            m.len() == 8 || m.len() == 256 * 1024 * 1024,
            "big{i} is {} bytes",
            m.len()
        );
    }
    assert!(d.join("usb/g/big0").exists(), "the source is untouched");
}
