#![cfg(unix)]
//! P3 Task 5: long management operations as jobs (`job.run`, payload/ava1/ava1_op.c and
//! payload/src/fs_jobs.c), driven over AVA1 against the real C. The filesystem operations
//! (delete, chmod -R, hash, crc32) run on real temporary trees; fsck, backup, cleanup and
//! sdk.scan run through the real op wrapper (mgmt_rpc.c) around stub legacy handlers.
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use ava1::gen::{self, JobEntry, JobListResult, JobRef, JobRun, Status};
use ava1::session::{connect, Session};
use ava1::wire::Message;
use ava1_ctest::*;
use common::*;

// The C server is a process-wide singleton: run this binary with `--test-threads=1`.

const OP_KIND: u8 = 4;

fn id(n: u8) -> [u8; 16] {
    [n; 16]
}

async fn run(s: &Session, job: [u8; 16], op: u8, args: &str) -> (u16, Option<Status>) {
    let body = JobRun {
        job_id: job,
        op,
        args: args.as_bytes().to_vec(),
    };
    let r = s
        .rpc(gen::METHOD_JOB_RUN, &body.to_bytes().unwrap())
        .await
        .unwrap();
    let st = (r.status == gen::STATUS_OK).then(|| Status::decode(&r.body).unwrap());
    (r.status, st)
}

async fn status(s: &Session, job: [u8; 16]) -> Status {
    let r = s
        .rpc(
            gen::METHOD_JOB_STATUS,
            &JobRef { job_id: job }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK, "job.status");
    Status::decode(&r.body).unwrap()
}

async fn status_code(s: &Session, job: [u8; 16]) -> u16 {
    s.rpc(
        gen::METHOD_JOB_STATUS,
        &JobRef { job_id: job }.to_bytes().unwrap(),
    )
    .await
    .unwrap()
    .status
}

async fn cancel(s: &Session, job: [u8; 16]) -> u16 {
    s.rpc(
        gen::METHOD_JOB_CANCEL,
        &JobRef { job_id: job }.to_bytes().unwrap(),
    )
    .await
    .unwrap()
    .status
}

async fn list(s: &Session) -> Vec<JobEntry> {
    let r = s.rpc(gen::METHOD_JOB_LIST, &[]).await.unwrap();
    assert_eq!(r.status, gen::STATUS_OK, "job.list");
    JobListResult::decode(&r.body).unwrap().jobs
}

async fn finished(s: &Session, job: [u8; 16]) -> Status {
    for _ in 0..600 {
        let st = status(s, job).await;
        if st.state.unwrap_or(0) != 0 {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the operation did not finish");
}

struct Rig {
    srv: CServer,
    me: Session,
    d: TempDir,
}

async fn rig(tag: &str) -> Rig {
    let d = dir(tag);
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, peers, "owner", calm())
        .await
        .unwrap();
    Rig { srv, me: s, d }
}

fn delete_args(p: &Path) -> String {
    serde_json::json!({ "path": p.to_str().unwrap() }).to_string()
}

fn count_files(p: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let t = e.file_type().unwrap();
            n += if t.is_dir() {
                count_files(&e.path())
            } else {
                1
            };
        }
    }
    n
}

// ---- delete ----

#[tokio::test(flavor = "multi_thread")]
async fn delete_tree_returns_immediately_and_reports_progress() {
    let r = rig("jr-del").await;
    let tree = r.d.join("g");
    write_tree(&tree, 5000, |_| 64);
    c_set_fsj_delay_us(300); // ~1.5 s of work: long enough to watch
    let t0 = Instant::now();
    let (code, st) = run(&r.me, id(1), gen::JOB_OP_DELETE, &delete_args(&tree)).await;
    assert_eq!(code, gen::STATUS_OK);
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "job.run took {:?}: it must answer at once",
        t0.elapsed()
    );
    assert_eq!(st.unwrap().state, Some(0), "still running");
    let (mut seen, mut last) = (Vec::new(), 0);
    let done = loop {
        let s = status(&r.me, id(1)).await;
        if s.state.unwrap_or(0) != 0 {
            break s; // read once: a delete is released when its terminal status was read
        }
        if s.files_done > last {
            seen.push(s.files_done);
            last = s.files_done;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert!(
        seen.len() >= 3,
        "progress should rise across polls: {seen:?}"
    );
    assert!(seen.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(done.state, Some(1));
    assert_eq!(done.files_total, 5000);
    assert_eq!(done.files_done, 5000);
    assert_eq!(done.bytes_total, 5000 * 64);
    assert_eq!(done.bytes_durable, 5000 * 64);
    assert!(!tree.exists(), "the tree is gone");
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_a_delete_midway_and_the_job_is_collected_after() {
    let r = rig("jr-cancel").await;
    let tree = r.d.join("g");
    write_tree(&tree, 3000, |_| 16);
    c_set_fsj_delay_us(500);
    assert_eq!(
        run(&r.me, id(2), gen::JOB_OP_DELETE, &delete_args(&tree))
            .await
            .0,
        gen::STATUS_OK
    );
    loop {
        if status(&r.me, id(2)).await.files_done >= 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let t0 = Instant::now();
    assert_eq!(cancel(&r.me, id(2)).await, gen::STATUS_OK);
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "cancel signals and returns: {:?}",
        t0.elapsed()
    );
    let st = finished(&r.me, id(2)).await;
    assert_eq!(st.state, Some(2), "the worker stops at its next entry");
    assert_eq!(st.code, Some(gen::ERR_CANCELLED));
    assert!(st.files_done < 3000);
    let left = count_files(&tree);
    assert!(left > 0 && left < 3000, "partly deleted: {left} left");
    assert!(tree.exists(), "the folder the user stopped stays");
    // A finished job is reaped a park age later; a running one never is.
    c_set_fsj_delay_us(0);
    c_reap_far();
    assert_eq!(
        status_code(&r.me, id(2)).await,
        gen::ERR_UNKNOWN_JOB,
        "unlisted"
    );
    // Another delete of the remains completes (the tree is deletable again).
    assert_eq!(
        run(&r.me, id(3), gen::JOB_OP_DELETE, &delete_args(&tree))
            .await
            .0,
        gen::STATUS_OK
    );
    assert_eq!(finished(&r.me, id(3)).await.state, Some(1));
    assert!(!tree.exists());
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_operation_is_never_reaped() {
    let r = rig("jr-noreap").await;
    let args = r#"{"device":"/dev/md1","loops":40}"#;
    assert_eq!(
        run(&r.me, id(4), gen::JOB_OP_FSCK, args).await.0,
        gen::STATUS_OK
    );
    c_reap_far();
    assert_eq!(
        status(&r.me, id(4)).await.state,
        Some(0),
        "still listed and running"
    );
    assert_eq!(finished(&r.me, id(4)).await.state, Some(1));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeat_job_run_is_idempotent() {
    let r = rig("jr-idem").await;
    let tree = r.d.join("g");
    write_tree(&tree, 400, |_| 8);
    c_set_fsj_delay_us(2000);
    let a = delete_args(&tree);
    assert_eq!(
        run(&r.me, id(5), gen::JOB_OP_DELETE, &a).await.0,
        gen::STATUS_OK
    );
    // The same id and parameters: the live job's status, not a second job.
    let (code, again) = run(&r.me, id(5), gen::JOB_OP_DELETE, &a).await;
    assert_eq!(code, gen::STATUS_OK);
    assert!(again.is_some());
    assert_eq!(
        list(&r.me)
            .await
            .iter()
            .filter(|j| j.kind == OP_KIND)
            .count(),
        1
    );
    // The same id with other parameters is refused, and so is another op.
    let other = delete_args(&r.d.join("elsewhere"));
    assert_eq!(
        run(&r.me, id(5), gen::JOB_OP_DELETE, &other).await.0,
        gen::ERR_PROTOCOL
    );
    assert_eq!(
        run(&r.me, id(5), gen::JOB_OP_CRC32, &a).await.0,
        gen::ERR_PROTOCOL
    );
    let done = finished(&r.me, id(5)).await;
    assert_eq!(done.state, Some(1));
    // Released once read: a repeat is simply a new run (the tree is gone, so it is a no-op success).
    let (code, _) = run(&r.me, id(5), gen::JOB_OP_DELETE, &a).await;
    assert_eq!(code, gen::STATUS_OK);
    assert_eq!(finished(&r.me, id(5)).await.state, Some(1));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_device_cannot_see_cancel_or_reuse_an_operation() {
    let d = dir("jr-owner");
    let (a_id, a_peers) = paired_client(&d.join("peers"));
    let (b_id, b_peers) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let a = connect(&srv.addr(), a_id, a_peers, "a", calm())
        .await
        .unwrap();
    let b = connect(&srv.addr(), b_id, b_peers, "b", calm())
        .await
        .unwrap();
    let args = r#"{"device":"/dev/md1","loops":30}"#;
    assert_eq!(
        run(&a, id(6), gen::JOB_OP_FSCK, args).await.0,
        gen::STATUS_OK
    );
    assert_eq!(status_code(&b, id(6)).await, gen::ERR_UNKNOWN_JOB);
    assert_eq!(cancel(&b, id(6)).await, gen::ERR_UNKNOWN_JOB);
    assert_eq!(
        run(&b, id(6), gen::JOB_OP_FSCK, args).await.0,
        gen::ERR_UNKNOWN_JOB
    );
    assert!(list(&b).await.is_empty(), "b lists only its own jobs");
    assert_eq!(list(&a).await.len(), 1);
    assert_eq!(finished(&a, id(6)).await.state, Some(1));
    drop(srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_midway_is_state_2_with_the_cause_and_the_rest_deleted() {
    let r = rig("jr-fail").await;
    if unsafe { libc_geteuid() } == 0 {
        return; // root ignores permission bits: the failure cannot be provoked
    }
    let tree = r.d.join("g");
    write_tree(&tree, 120, |_| 8);
    let stuck = tree.join("d05");
    let n_stuck = count_files(&stuck);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(
        run(&r.me, id(7), gen::JOB_OP_DELETE, &delete_args(&tree))
            .await
            .0,
        gen::STATUS_OK
    );
    let st = finished(&r.me, id(7)).await;
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(st.state, Some(2));
    assert_eq!(st.code, Some(gen::ERR_IO));
    assert!(st
        .current
        .as_deref()
        .unwrap()
        .starts_with("fs_delete_failed"));
    assert_eq!(
        count_files(&tree),
        n_stuck,
        "everything else was removed, best effort"
    );
    drop(r.srv);
}

extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_refuses_outside_policy_a_mount_point_and_a_bad_request() {
    let r = rig("jr-policy").await;
    let denied = r.d.join("ps5-denied/x");
    write_tree(&denied, 3, |_| 4);
    let (_, _) = run(&r.me, id(8), gen::JOB_OP_DELETE, &delete_args(&denied)).await;
    let st = finished(&r.me, id(8)).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_PATH)));
    assert_eq!(st.current.as_deref(), Some("fs_delete_path_not_allowed"));
    assert_eq!(count_files(&denied), 3);
    // A path on another device than its parent is a mount point: refused, nothing removed.
    let mounted = r.d.join("m");
    write_tree(&mounted, 3, |_| 4);
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(0) };
    run(&r.me, id(9), gen::JOB_OP_DELETE, &delete_args(&mounted)).await;
    let st = finished(&r.me, id(9)).await;
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(1) };
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_PATH)));
    assert_eq!(st.current.as_deref(), Some("fs_delete_path_is_mount_point"));
    assert_eq!(count_files(&mounted), 3);
    // No path at all, and a relative one.
    run(&r.me, id(10), gen::JOB_OP_DELETE, "{}").await;
    assert_eq!(finished(&r.me, id(10)).await.code, Some(gen::ERR_PROTOCOL));
    run(&r.me, id(11), gen::JOB_OP_DELETE, r#"{"path":"data/x"}"#).await;
    assert_eq!(finished(&r.me, id(11)).await.code, Some(gen::ERR_PATH));
    // A path that is already gone is a success.
    run(
        &r.me,
        id(12),
        gen::JOB_OP_DELETE,
        &delete_args(&r.d.join("nothing")),
    )
    .await;
    assert_eq!(finished(&r.me, id(12)).await.state, Some(1));
    // A path with quotes and a backslash survives the JSON round trip.
    let odd = r.d.join("we\"ird\\name");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join("f"), b"x").unwrap();
    run(&r.me, id(13), gen::JOB_OP_DELETE, &delete_args(&odd)).await;
    assert_eq!(finished(&r.me, id(13)).await.state, Some(1));
    assert!(!odd.exists());
    drop(r.srv);
}

// ---- chmod -R, hash, crc32 ----

#[tokio::test(flavor = "multi_thread")]
async fn chmod_recursive_applies_to_every_entry_and_reports_progress() {
    let r = rig("jr-chmod").await;
    let tree = r.d.join("g");
    write_tree(&tree, 200, |_| 8);
    c_set_fsj_delay_us(200);
    let a = serde_json::json!({ "path": tree.to_str().unwrap(), "mode": "0700" }).to_string();
    assert_eq!(
        run(&r.me, id(20), gen::JOB_OP_CHMOD_R, &a).await.0,
        gen::STATUS_OK
    );
    let st = finished(&r.me, id(20)).await;
    assert_eq!(st.state, Some(1));
    assert_eq!(
        (st.files_total, st.files_done),
        (200, 200),
        "every file counted, folders not"
    );
    for e in std::fs::read_dir(&tree).unwrap().flatten() {
        assert_eq!(e.metadata().unwrap().permissions().mode() & 0o7777, 0o700);
    }
    let f = tree.join("d00/f00000");
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    // No mode, and a path outside the policy.
    run(&r.me, id(21), gen::JOB_OP_CHMOD_R, &delete_args(&tree)).await;
    assert_eq!(finished(&r.me, id(21)).await.code, Some(gen::ERR_PROTOCOL));
    let denied =
        serde_json::json!({ "path": r.d.join("ps5-denied/a").to_str().unwrap(), "mode": "0777" })
            .to_string();
    run(&r.me, id(22), gen::JOB_OP_CHMOD_R, &denied).await;
    assert_eq!(finished(&r.me, id(22)).await.code, Some(gen::ERR_PATH));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_result_rides_in_status_ext() {
    let r = rig("jr-hash").await;
    let f = r.d.join("big.bin");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
    std::fs::write(&f, &data).unwrap();
    let a = serde_json::json!({ "path": f.to_str().unwrap() }).to_string();
    let (code, first) = run(&r.me, id(30), gen::JOB_OP_HASH, &a).await;
    assert_eq!(code, gen::STATUS_OK);
    assert!(first.unwrap().result.is_none(), "no result while running");
    let st = finished(&r.me, id(30)).await;
    assert_eq!(st.state, Some(1));
    let v: serde_json::Value = serde_json::from_slice(&st.result.expect("result")).unwrap();
    assert_eq!(v["hash"], blake3::hash(&data).to_hex().to_string());
    assert_eq!(v["size"], 300_000);
    assert_eq!(v["path"], f.to_str().unwrap());
    assert_eq!(st.bytes_total, 300_000);
    // A hash is repeatable, but its finished job stays listed for the grace after its terminal
    // status was first delivered (the release is covered by the grace test below).
    assert_eq!(status(&r.me, id(30)).await.state, Some(1));
    // A folder is not hashable, a missing file fails with a cause.
    run(&r.me, id(31), gen::JOB_OP_HASH, &delete_args(&r.d)).await;
    let st = finished(&r.me, id(31)).await;
    assert_eq!(
        (st.state, st.current.as_deref()),
        (Some(2), Some("fs_hash_not_regular_file"))
    );
    run(
        &r.me,
        id(32),
        gen::JOB_OP_HASH,
        &delete_args(&r.d.join("nope")),
    )
    .await;
    assert_eq!(
        finished(&r.me, id(32)).await.current.as_deref(),
        Some("fs_hash_stat_failed")
    );
    drop(r.srv);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[tokio::test(flavor = "multi_thread")]
async fn crc32_of_a_large_file_is_a_job_that_can_be_cancelled() {
    let r = rig("jr-crc").await;
    let f = r.d.join("c.bin");
    let data: Vec<u8> = (0..1_000_000u32).map(|i| (i ^ (i >> 8)) as u8).collect();
    std::fs::write(&f, &data).unwrap();
    let a = serde_json::json!({ "path": f.to_str().unwrap() }).to_string();
    assert_eq!(
        run(&r.me, id(40), gen::JOB_OP_CRC32, &a).await.0,
        gen::STATUS_OK
    );
    let st = finished(&r.me, id(40)).await;
    let v: serde_json::Value = serde_json::from_slice(&st.result.unwrap()).unwrap();
    assert_eq!(v["crc32"], crc32(&data));
    assert_eq!(v["size"], 1_000_000);
    // Slow it down (every 64 KiB block waits), then cancel after some progress.
    c_set_fsj_delay_us(30_000);
    assert_eq!(
        run(&r.me, id(41), gen::JOB_OP_CRC32, &a).await.0,
        gen::STATUS_OK
    );
    loop {
        if status(&r.me, id(41)).await.bytes_durable > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(cancel(&r.me, id(41)).await, gen::STATUS_OK);
    let st = finished(&r.me, id(41)).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_CANCELLED)));
    assert!(st.bytes_durable < 1_000_000);
    assert!(st.result.is_none());
    drop(r.srv);
}

// ---- operations that wrap a legacy handler (stubs behind the real wrapper) ----

#[tokio::test(flavor = "multi_thread")]
async fn fsck_keeps_its_ok_false_body_as_the_answer_and_a_frame_error_is_a_failure() {
    let r = rig("jr-fsck").await;
    run(&r.me, id(50), gen::JOB_OP_FSCK, r#"{"device":"/dev/md1"}"#).await;
    let st = finished(&r.me, id(50)).await;
    assert_eq!(st.state, Some(1));
    assert!(String::from_utf8_lossy(&st.result.unwrap()).contains("\"ok\":true"));
    // A dirty volume: the operation ran, its body says ok:false with the code the caller reads.
    run(
        &r.me,
        id(51),
        gen::JOB_OP_FSCK,
        r#"{"device":"/dev/md1","dirty":1}"#,
    )
    .await;
    let st = finished(&r.me, id(51)).await;
    assert_eq!(st.state, Some(1));
    let v: serde_json::Value = serde_json::from_slice(&st.result.unwrap()).unwrap();
    assert_eq!(
        (v["ok"].as_bool(), v["code"].as_i64()),
        (Some(false), Some(3))
    );
    // An ERROR frame fails the job with its token as the cause.
    run(&r.me, id(52), gen::JOB_OP_FSCK, r#"{"error":1}"#).await;
    let st = finished(&r.me, id(52)).await;
    assert_eq!(st.state, Some(2));
    assert_eq!(
        st.current.as_deref(),
        Some("libSceFsInternalForVsh_unavailable")
    );
    assert_eq!(st.code, Some(gen::ERR_INTERNAL));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_snapshot_reports_progress_and_cancel_stops_it() {
    let r = rig("jr-backup").await;
    run(
        &r.me,
        id(60),
        gen::JOB_OP_BACKUP_SNAPSHOT,
        r#"{"tag":"t","path":"/data/x","loops":15}"#,
    )
    .await;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let s = status(&r.me, id(60)).await;
        if s.state.unwrap_or(0) != 0 {
            assert_eq!(s.state, Some(1));
            assert!(String::from_utf8_lossy(&s.result.unwrap()).contains("\"files\":2"));
            break;
        }
        seen.insert(s.files_done);
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(seen.len() >= 3, "progress rose: {seen:?}");
    // Cancel mid-way: state 2 / cancelled, the cause token kept.
    run(
        &r.me,
        id(61),
        gen::JOB_OP_BACKUP_SNAPSHOT,
        r#"{"tag":"t","path":"/data/x","loops":500}"#,
    )
    .await;
    loop {
        if status(&r.me, id(61)).await.files_done >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(cancel(&r.me, id(61)).await, gen::STATUS_OK);
    let st = finished(&r.me, id(61)).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_CANCELLED)));
    assert_eq!(st.current.as_deref(), Some("backup_cancelled"));
    assert!(st.files_done < 500);
    // A failure mid-operation: the frame error's token maps to ERR_IO.
    run(&r.me, id(62), gen::JOB_OP_BACKUP_SNAPSHOT, r#"{"fail":1}"#).await;
    let st = finished(&r.me, id(62)).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_IO)));
    assert_eq!(st.current.as_deref(), Some("backup_snapshot_io_error"));
    // Restore answers ok:false in a normal frame: the body is kept.
    run(
        &r.me,
        id(63),
        gen::JOB_OP_BACKUP_RESTORE,
        r#"{"tag":"t","timestamp":1}"#,
    )
    .await;
    let st = finished(&r.me, id(63)).await;
    assert_eq!(st.state, Some(1));
    assert!(String::from_utf8_lossy(&st.result.unwrap()).contains("restore failed"));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_and_a_100_kib_sdk_scan_result_come_back_whole() {
    let r = rig("jr-sdk").await;
    run(&r.me, id(70), gen::JOB_OP_CLEANUP, r#"{"path":"/data/x"}"#).await;
    let st = finished(&r.me, id(70)).await;
    assert!(String::from_utf8_lossy(&st.result.unwrap()).contains("removed_files"));
    run(&r.me, id(71), gen::JOB_OP_SDK_SCAN, "").await;
    let st = finished(&r.me, id(71)).await;
    let body = st.result.unwrap();
    assert_eq!(body.len(), 100 * 1024);
    assert!(body.starts_with(b"{\"titles\":\"") && body.ends_with(b"\"}"));
    drop(r.srv);
}

// ---- the job table ----

#[tokio::test(flavor = "multi_thread")]
async fn status_survives_a_reconnect() {
    let d = dir("jr-reconnect");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let first = connect(&srv.addr(), me.clone(), peers.clone(), "e1", calm())
        .await
        .unwrap();
    let tree = d.join("g");
    write_tree(&tree, 800, |_| 8);
    c_set_fsj_delay_us(1500);
    assert_eq!(
        run(&first, id(80), gen::JOB_OP_DELETE, &delete_args(&tree))
            .await
            .0,
        gen::STATUS_OK
    );
    let before = loop {
        let s = status(&first, id(80)).await;
        if s.files_done > 50 {
            break s.files_done;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    drop(first); // the session ends; the operation runs on
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = connect(&srv.addr(), me, peers, "e2", calm()).await.unwrap();
    let s = status(&second, id(80)).await;
    assert!(
        s.files_done >= before,
        "the new session reads the same job: {} vs {before}",
        s.files_done
    );
    assert!(list(&second).await.iter().any(|j| j.kind == OP_KIND));
    // Re-issuing the run from the new session is the idempotent answer, not a second delete.
    let (code, st) = run(&second, id(80), gen::JOB_OP_DELETE, &delete_args(&tree)).await;
    assert_eq!(code, gen::STATUS_OK);
    assert!(st.unwrap().files_done >= before);
    assert_eq!(finished(&second, id(80)).await.state, Some(1));
    assert!(!tree.exists());
    drop(srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_eight_operations_run_at_once_and_an_unknown_op_is_refused() {
    let r = rig("jr-cap").await;
    let slow = r#"{"device":"/dev/md1","loops":200}"#;
    for n in 0..8u8 {
        assert_eq!(
            run(&r.me, id(100 + n), gen::JOB_OP_FSCK, slow).await.0,
            gen::STATUS_OK
        );
    }
    let (code, _) = run(&r.me, id(120), gen::JOB_OP_FSCK, slow).await;
    assert_eq!(code, gen::ERR_BUSY, "the ninth is refused while eight run");
    assert_eq!(list(&r.me).await.len(), 8);
    for n in 0..8u8 {
        assert_eq!(cancel(&r.me, id(100 + n)).await, gen::STATUS_OK);
    }
    for n in 0..8u8 {
        assert_eq!(
            finished(&r.me, id(100 + n)).await.code,
            Some(gen::ERR_CANCELLED)
        );
    }
    // Slots free up as they end.
    assert_eq!(
        run(&r.me, id(121), gen::JOB_OP_FSCK, r#"{"device":"/dev/md1"}"#)
            .await
            .0,
        gen::STATUS_OK
    );
    assert_eq!(run(&r.me, id(122), 99, "{}").await.0, gen::ERR_PROTOCOL);
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_run_with_an_oversize_or_malformed_body_is_a_protocol_error() {
    let r = rig("jr-bad").await;
    let big = "x".repeat(61 * 1024);
    assert_eq!(
        run(&r.me, id(130), gen::JOB_OP_DELETE, &big).await.0,
        gen::ERR_PROTOCOL
    );
    let s = r.me.rpc(gen::METHOD_JOB_RUN, &[1, 2, 3]).await.unwrap();
    assert_eq!(s.status, gen::ERR_PROTOCOL);
    assert!(list(&r.me).await.is_empty());
    drop(r.srv);
}

// ---- fix round 1 ----

/// Starts a delete and returns how it ended.
async fn delete_outcome(r: &Rig, n: u8, path: &str) -> Status {
    run(
        &r.me,
        id(n),
        gen::JOB_OP_DELETE,
        &serde_json::json!({ "path": path }).to_string(),
    )
    .await;
    finished(&r.me, id(n)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn roots_and_trailing_slash_spellings_are_refused_and_nothing_is_deleted() {
    let r = rig("jr-roots").await;
    // The usual spellings of a drive or a writable root, none of which may be emptied.
    let mut n = 200u8;
    for p in [
        "/mnt/usb0/",
        "/data/",
        "/user/",
        "/mnt/usb0//",
        "/mnt/usb0/.",
        "/data/./",
        "/data",
        "/mnt/ext1",
        "//data//",
        "/",
        "/mnt",
        "/mnt/usb0/./",
    ] {
        n += 1;
        let st = delete_outcome(&r, n, p).await;
        assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_PATH)), "{p}");
    }
    // A mount point named with a trailing slash or a dot is still a mount point: the parent is the
    // folder above it, not the path itself.
    let m = r.d.join("m");
    write_tree(&m, 3, |_| 4);
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(2) };
    for (i, spelling) in [
        format!("{}/", m.display()),
        format!("{}//", m.display()),
        format!("{}/.", m.display()),
    ]
    .iter()
    .enumerate()
    {
        let st = delete_outcome(&r, 230 + i as u8, spelling).await;
        assert_eq!(
            (st.state, st.code),
            (Some(2), Some(gen::ERR_PATH)),
            "{spelling}"
        );
        assert_eq!(count_files(&m), 3, "{spelling}");
    }
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(1) };
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_device_answer_refuses_the_delete() {
    let r = rig("jr-unknown-dev").await;
    let t = r.d.join("t");
    write_tree(&t, 3, |_| 4);
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(-1) };
    let st = delete_outcome(&r, 240, t.to_str().unwrap()).await;
    unsafe { ava1_ctest::ffi::ava1_test_set_same_device(1) };
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_PATH)));
    assert_eq!(count_files(&t), 3);
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_hash_ops_never_fill_the_table_a_full_table_reclaims_a_delivered_slot() {
    let r = rig("jr-slots").await;
    let f = r.d.join("h.bin");
    std::fs::write(&f, b"hello").unwrap();
    let a = serde_json::json!({ "path": f.to_str().unwrap() }).to_string();
    for n in 0..200u32 {
        let mut job = [0u8; 16];
        job[..4].copy_from_slice(&n.to_le_bytes());
        job[15] = 0x77;
        let (code, st) = run(&r.me, job, gen::JOB_OP_HASH, &a).await;
        assert_eq!(code, gen::STATUS_OK, "op {n}: the table must never fill");
        let st = match st.filter(|s| s.state.unwrap_or(0) != 0) {
            Some(s) => s,
            None => finished(&r.me, job).await,
        };
        assert_eq!(st.state, Some(1), "op {n}");
    }
    // The table never filled although every op was read within its grace: a full table
    // reclaims the slot of the op delivered longest ago.
    assert!(list(&r.me).await.len() <= 32);
    // A backup is not repeatable, so its result is kept for the short done-age instead.
    run(
        &r.me,
        id(250),
        gen::JOB_OP_BACKUP_SNAPSHOT,
        r#"{"tag":"t","path":"/data/x"}"#,
    )
    .await;
    finished(&r.me, id(250)).await;
    assert_eq!(
        status(&r.me, id(250)).await.state,
        Some(1),
        "still readable"
    );
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partial_delete_failure_names_the_first_path_and_the_entries_left() {
    let r = rig("jr-firstfail").await;
    if unsafe { libc_geteuid() } == 0 {
        return;
    }
    let tree = r.d.join("g");
    write_tree(&tree, 120, |_| 8);
    let stuck = tree.join("d05");
    let n_stuck = count_files(&stuck);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).unwrap();
    let st = delete_outcome(&r, 241, tree.to_str().unwrap()).await;
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cur = st.current.unwrap();
    assert!(cur.starts_with("fs_delete_failed"), "{cur}");
    assert!(
        cur.contains("d05/f"),
        "names a path inside the stuck folder: {cur}"
    );
    assert!(
        cur.contains(&format!("{n_stuck} left")),
        "counts the entries left: {cur}"
    );
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walkers_never_enter_a_nested_mount() {
    let r = rig("jr-nested").await;
    let tree = r.d.join("g");
    write_tree(&tree, 30, |_| 4);
    let inner = tree.join("mnt");
    write_tree(&inner, 5, |_| 4);
    c_set_cross_name("mnt"); // the folder named mnt reports another device
    let st = delete_outcome(&r, 242, tree.to_str().unwrap()).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_IO)));
    let cur = st.current.unwrap();
    assert!(cur.starts_with("fs_delete_failed: mount "), "{cur}");
    assert!(cur.contains("g/mnt (1 left)"), "{cur}");
    assert_eq!(
        count_files(&inner),
        5,
        "nothing below the mount was touched"
    );
    assert_eq!(count_files(&tree), 5, "the rest of the tree is gone");
    // chmod -R skips it too.
    let a = serde_json::json!({ "path": tree.to_str().unwrap(), "mode": "0700" }).to_string();
    run(&r.me, id(243), gen::JOB_OP_CHMOD_R, &a).await;
    let st = finished(&r.me, id(243)).await;
    assert_eq!(st.state, Some(2));
    assert!(st.current.unwrap().contains("mount"));
    let f = std::fs::read_dir(&inner).unwrap().flatten().next().unwrap();
    assert_ne!(
        f.metadata().unwrap().permissions().mode() & 0o7777,
        0o700,
        "not chmod'd inside the mount"
    );
    c_set_cross_name("");
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_atomic_copy_never_truncates_the_destination_and_a_cancel_removes_only_its_temp() {
    let r = rig("jr-atomic").await;
    let src = r.d.join("backup.bin");
    let dst = r.d.join("live.bin");
    let new: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &new).unwrap();
    std::fs::write(&dst, b"the user's live file").unwrap();
    // Cancelled after two 64 KiB blocks: the live file is exactly as it was, no temp is left.
    c_set_fsj_delay_us(1000);
    assert_eq!(c_copy_atomic(&src, &dst, 2), -2);
    assert_eq!(std::fs::read(&dst).unwrap(), b"the user's live file");
    let leftovers = |d: &Path| {
        std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".part"))
            .count()
    };
    assert_eq!(leftovers(&r.d), 0);
    // A missing source fails the same way.
    assert_eq!(c_copy_atomic(&r.d.join("nope"), &dst, -1), -1);
    assert_eq!(std::fs::read(&dst).unwrap(), b"the user's live file");
    // Complete: the new bytes land in one rename, the mtime is the source's, no temp remains.
    assert_eq!(c_copy_atomic(&src, &dst, -1), 0);
    assert_eq!(std::fs::read(&dst).unwrap(), new);
    assert_eq!(leftovers(&r.d), 0);
    let secs = |p: &Path| {
        std::fs::metadata(p)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(secs(&dst), secs(&src));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_atomic_copy_keeps_the_source_mode_and_a_nul_in_a_path_is_refused() {
    let r = rig("jr-mode").await;
    let src = r.d.join("tool.sh");
    let dst = r.d.join("live.sh");
    std::fs::write(&src, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&dst, b"old").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(c_copy_atomic(&src, &dst, -1), 0);
    assert_eq!(
        std::fs::metadata(&dst).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    // A fresh destination gets the source's mode too (not the temp file's 0644).
    let fresh = r.d.join("fresh.sh");
    assert_eq!(c_copy_atomic(&src, &fresh, -1), 0);
    assert_eq!(
        std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    // \u0000 would cut the path at the NUL (and delete the parent folder's sibling): refused.
    let victim = r.d.join("victim");
    write_tree(&victim, 2, |_| 4);
    let args = format!(r#"{{"path":"{}\u0000/x"}}"#, victim.display());
    run(&r.me, id(244), gen::JOB_OP_DELETE, &args).await;
    let st = finished(&r.me, id(244)).await;
    assert_eq!((st.state, st.code), (Some(2), Some(gen::ERR_PROTOCOL)));
    assert_eq!(count_files(&victim), 2);
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_op_that_finishes_while_its_running_reply_is_built_stays_readable() {
    // The reply to job.run is encoded after the op has finished, but the "was it finished" read
    // came first: the job must stay listed, or the engine's next job.status gets ERR_UNKNOWN_JOB.
    let r = rig("jr-race").await;
    let f = r.d.join("race.bin");
    std::fs::write(&f, b"hello").unwrap();
    let a = serde_json::json!({ "path": f.to_str().unwrap() }).to_string();
    unsafe { ffi::ava1_test_op_hold_reply_until_finished(1) };
    let (code, _) = run(&r.me, id(201), gen::JOB_OP_HASH, &a).await;
    unsafe { ffi::ava1_test_op_hold_reply_until_finished(0) };
    assert_eq!(code, gen::STATUS_OK);
    let st = status(&r.me, id(201)).await; // ERR_UNKNOWN_JOB before the fix
    assert_eq!(st.state, Some(1));
    drop(r.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_op_stays_listed_for_a_grace_then_is_released_on_the_next_read() {
    // Review 004 O2: a lost job.run reply must still get the stored answer, so a repeat of
    // job.run within the grace does not re-run the op; after the grace the next read releases it.
    let r = rig("jr-grace").await;
    r.srv.knob("park_ms", 400); // the grace is min(park age, 10 s)
    let f = r.d.join("g.bin");
    std::fs::write(&f, b"first").unwrap();
    let a = serde_json::json!({ "path": f.to_str().unwrap() }).to_string();
    run(&r.me, id(210), gen::JOB_OP_HASH, &a).await;
    let first = finished(&r.me, id(210)).await; // the first terminal delivery starts the grace
    let h1 = serde_json::from_slice::<serde_json::Value>(&first.result.unwrap()).unwrap()["hash"]
        .clone();
    std::fs::write(&f, b"second, different").unwrap();
    // The repeat is answered from the stored job: the old hash, not a re-run on the new bytes.
    let (code, again) = run(&r.me, id(210), gen::JOB_OP_HASH, &a).await;
    assert_eq!(code, gen::STATUS_OK);
    let again = again.unwrap();
    assert_eq!(again.state, Some(1));
    let h2 = serde_json::from_slice::<serde_json::Value>(&again.result.unwrap()).unwrap()["hash"]
        .clone();
    assert_eq!(h1, h2, "nothing ran twice inside the grace");
    assert_eq!(h1, blake3::hash(b"first").to_hex().to_string());
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Past the grace, a read releases it (or the reaper already did).
    let _ = status_code(&r.me, id(210)).await;
    assert_eq!(status_code(&r.me, id(210)).await, gen::ERR_UNKNOWN_JOB);
    drop(r.srv);
}
