#![allow(dead_code, unused_imports)]
#![cfg(unix)]
//! Review S2 (carry-forward from round 2): the job.run filesystem ops refuse the AVA1 trust store and its
//! ancestors as roots. Same harness as job_run.rs.
//! (originally) P3 Task 5: long management operations as jobs (`job.run`, payload/ava1/ava1_op.c and
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
    d: std::path::PathBuf,
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

fn args_mode(p: &Path) -> String {
    serde_json::json!({ "path": p.to_str().unwrap(), "mode": "0777" }).to_string()
}

/// job.run DELETE / CHMOD_R / HASH / CRC32 of the store, of a file in it and of every ancestor are refused
/// with ERR_PATH and change nothing; an unrelated sibling still works.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn s2_job_ops_refuse_the_trust_store_and_its_ancestors() {
    let _g = CServer::lock_for_shim_tests();
    let r = rig("jr-s2").await;
    let top = r.d.join("console/ps5upload");
    let store = top.join("ava");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("identity"), b"secret").unwrap();
    std::fs::write(store.join("peers"), b"peers").unwrap();
    std::fs::write(r.d.join("console/sibling.txt"), b"x").unwrap();
    let store = store.canonicalize().unwrap();
    let top = top.canonicalize().unwrap();
    let console = top.parent().unwrap().to_path_buf();
    c_set_protected(Some(&store));
    let mut n = 40u8;
    let mut refused = |op: u8, args: String| {
        n += 1;
        (n, op, args)
    };
    let cases = vec![
        refused(gen::JOB_OP_DELETE, delete_args(&top)),
        refused(gen::JOB_OP_DELETE, delete_args(&store)),
        refused(gen::JOB_OP_DELETE, delete_args(&store.join("peers"))),
        refused(gen::JOB_OP_DELETE, delete_args(&console)),
        refused(gen::JOB_OP_CHMOD_R, args_mode(&top)),
        refused(gen::JOB_OP_CHMOD_R, args_mode(&store)),
        refused(gen::JOB_OP_HASH, delete_args(&store.join("identity"))),
        refused(gen::JOB_OP_CRC32, delete_args(&store.join("identity"))),
    ];
    for (j, op, args) in cases {
        let (code, _) = run(&r.me, id(j), op, &args).await;
        let st = if code == gen::STATUS_OK {
            finished(&r.me, id(j)).await
        } else {
            // refused at open: the code is the answer
            Status {
                state: Some(2),
                code: Some(code),
                ..Default::default()
            }
        };
        assert_eq!(
            (st.state, st.code),
            (Some(2), Some(gen::ERR_PATH)),
            "op {op} {args} must be refused"
        );
    }
    assert_eq!(std::fs::read(store.join("identity")).unwrap(), b"secret");
    assert_eq!(std::fs::read(store.join("peers")).unwrap(), b"peers");
    assert!(console.join("sibling.txt").exists());
    // an unrelated sibling is still deletable
    run(
        &r.me,
        id(99),
        gen::JOB_OP_DELETE,
        &delete_args(&console.join("sibling.txt")),
    )
    .await;
    assert_eq!(finished(&r.me, id(99)).await.state, Some(1));
    assert!(!console.join("sibling.txt").exists());
    c_set_protected(None);
    drop(r.srv);
}
