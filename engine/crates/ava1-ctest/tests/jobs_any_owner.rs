#![cfg(unix)]
//! Console web UI Task 5: the on-console page runs under its own identity and lists and
//! cancels every job the helper runs, whoever started it (`ava1_jobs_list_all`,
//! `ava1_jobs_owner_is`, `ava1_job_cancel_any`). Paired PCs keep their own-jobs-only view.
mod common;

use std::time::Duration;

use ava1::gen::{self, JobCopy, JobEntry, JobListResult, JobRef, JobRun};
use ava1::session::{connect, Session};
use ava1::wire::Message;
use ava1_ctest::*;
use common::*;

// The C server is a process-wide singleton. nextest runs each test in its own process, but
// `cargo test` (the coverage job) runs them as threads of one process, where one test filling
// the 32-slot table could evict another test's finished jobs. Each test takes this lock first.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const OP_KIND: u8 = 4;
/// The console page's fixed identity (constraints.md): ASCII, zero-padded to 32 bytes.
const CONSOLE: [u8; 32] = *b"ps5upload-console-webui\0\0\0\0\0\0\0\0\0";

async fn list(s: &Session) -> Vec<JobEntry> {
    let r = s.rpc(gen::METHOD_JOB_LIST, &[]).await.unwrap();
    assert_eq!(r.status, gen::STATUS_OK, "job.list");
    JobListResult::decode(&r.body).unwrap().jobs
}

fn list_all(me: &[u8; 32]) -> Vec<JobEntry> {
    let body = c_jobs_list_all(me).expect("list_all");
    JobListResult::decode(&body).unwrap().jobs
}

async fn code(s: &Session, method: u16, job: [u8; 16]) -> u16 {
    s.rpc(method, &JobRef { job_id: job }.to_bytes().unwrap())
        .await
        .unwrap()
        .status
}

struct Two {
    srv: CServer,
    a: Session,
    a_key: [u8; 32],
    b: Session,
    b_key: [u8; 32],
    d: TempDir,
}

async fn two(tag: &str, fsync_delay_us: u32) -> Two {
    let d = dir(tag);
    let (a_id, a_peers) = paired_client(&d.join("peers"));
    let (b_id, b_peers) = paired_client(&d.join("peers"));
    let (a_key, b_key) = (a_id.public(), b_id.public());
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        fsync_delay_us,
    );
    let a = connect(&srv.addr(), a_id, a_peers, "a", calm())
        .await
        .unwrap();
    let b = connect(&srv.addr(), b_id, b_peers, "b", calm())
        .await
        .unwrap();
    Two {
        srv,
        a,
        a_key,
        b,
        b_key,
        d,
    }
}

async fn run_fsck(s: &Session, job: [u8; 16]) -> u16 {
    let body = JobRun {
        job_id: job,
        op: gen::JOB_OP_FSCK,
        args: br#"{"device":"/dev/md1","loops":30}"#.to_vec(),
    };
    s.rpc(gen::METHOD_JOB_RUN, &body.to_bytes().unwrap())
        .await
        .unwrap()
        .status
}

async fn copy(s: &Session, job: [u8; 16], src: &std::path::Path, dest: &std::path::Path) -> u16 {
    let body = JobCopy {
        job_id: job,
        src: src.to_str().unwrap().into(),
        dest: dest.to_str().unwrap().into(),
        flags: 0,
    };
    s.rpc(gen::METHOD_JOB_COPY, &body.to_bytes().unwrap())
        .await
        .unwrap()
        .status
}

#[tokio::test(flavor = "multi_thread")]
async fn list_all_includes_other_owners_jobs() {
    let _serial = SERIAL.lock().await;
    let t = two("any-list", 0).await;
    let op = [0xa1; 16];
    assert_eq!(run_fsck(&t.a, op).await, gen::STATUS_OK);
    // B, a paired PC, still sees only its own jobs over JOB_LIST.
    assert!(
        list(&t.b).await.is_empty(),
        "JOB_LIST as b lists only b's jobs"
    );
    // The console page sees a's operation, whichever identity asks.
    for me in [&t.b_key, &CONSOLE] {
        let all = list_all(me);
        assert_eq!(all.len(), 1, "list_all shows every owner's job");
        assert_eq!(all[0].job_id, op);
        assert_eq!(all[0].kind, OP_KIND);
    }
    // Ownership is reported beside the list, not in it.
    assert_eq!(c_jobs_owner_is(&op, &t.a_key), 1);
    assert_eq!(c_jobs_owner_is(&op, &t.b_key), 0);
    assert_eq!(c_jobs_owner_is(&op, &CONSOLE), 0);
    assert_eq!(c_jobs_owner_is(&[0xee; 16], &t.a_key), 0, "absent job");
    drop(t.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_any_cancels_other_owners_copy() {
    let _serial = SERIAL.lock().await;
    let t = two("any-cancel", 3000).await;
    write_tree(&t.d.join("usb/g"), 3000, |_| 2048);
    let job = [0xa2; 16];
    assert_eq!(
        copy(&t.a, job, &t.d.join("usb/g"), &t.d.join("data/g")).await,
        gen::STATUS_OK
    );
    assert!(list_all(&CONSOLE).iter().any(|e| e.job_id == job));
    // B over the RPC still cannot touch it ...
    assert_eq!(
        code(&t.b, gen::METHOD_JOB_CANCEL, job).await,
        gen::ERR_UNKNOWN_JOB
    );
    // ... the console page can.
    assert_eq!(c_job_cancel_any(&job), gen::STATUS_OK as i32);
    let mut gone = false;
    for _ in 0..200 {
        if code(&t.a, gen::METHOD_JOB_STATUS, job).await == gen::ERR_UNKNOWN_JOB {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "the cancelled copy is unlisted for its owner");
    assert!(!list_all(&CONSOLE).iter().any(|e| e.job_id == job));
    assert_eq!(c_job_cancel_any(&job), gen::ERR_UNKNOWN_JOB as i32);
    assert_eq!(c_job_cancel_any(&[0xef; 16]), gen::ERR_UNKNOWN_JOB as i32);
    drop(t.srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn job_list_rpc_unchanged_for_pcs() {
    let _serial = SERIAL.lock().await;
    let t = two("any-rpc", 0).await;
    let (ja, jb) = ([0xa3; 16], [0xa4; 16]);
    assert_eq!(run_fsck(&t.a, ja).await, gen::STATUS_OK);
    assert_eq!(run_fsck(&t.b, jb).await, gen::STATUS_OK);
    let la: Vec<_> = list(&t.a).await.iter().map(|e| e.job_id).collect();
    let lb: Vec<_> = list(&t.b).await.iter().map(|e| e.job_id).collect();
    assert_eq!(la, vec![ja]);
    assert_eq!(lb, vec![jb]);
    // Each PC's own status/cancel still works; the other's is still refused.
    assert_eq!(
        code(&t.b, gen::METHOD_JOB_STATUS, ja).await,
        gen::ERR_UNKNOWN_JOB
    );
    assert_eq!(
        code(&t.a, gen::METHOD_JOB_CANCEL, jb).await,
        gen::ERR_UNKNOWN_JOB
    );
    assert_eq!(code(&t.a, gen::METHOD_JOB_CANCEL, ja).await, gen::STATUS_OK);
    assert_eq!(code(&t.b, gen::METHOD_JOB_CANCEL, jb).await, gen::STATUS_OK);
    // The console page sees both.
    let mut all: Vec<_> = list_all(&CONSOLE).iter().map(|e| e.job_id).collect();
    all.sort();
    assert_eq!(all, vec![ja, jb]);
    drop(t.srv);
}

const FSCK_NOW: &[u8] = br#"{"device":"/dev/md1","loops":0}"#;

fn fsck_body(job: [u8; 16]) -> Vec<u8> {
    JobRun {
        job_id: job,
        op: gen::JOB_OP_FSCK,
        args: FSCK_NOW.to_vec(),
    }
    .to_bytes()
    .unwrap()
}

/// `job.run` of a quick fsck as the console page (the page's routes call the data layer with its
/// identity set on the thread): the status.
fn run_as_console(job: [u8; 16]) -> i32 {
    c_data_rpc_as(&CONSOLE, gen::METHOD_JOB_RUN, &fsck_body(job)).0
}

/// `job.copy` as the console page.
fn copy_as_console(job: [u8; 16], src: &std::path::Path, dest: &std::path::Path) -> i32 {
    let body = JobCopy {
        job_id: job,
        src: src.to_str().unwrap().into(),
        dest: dest.to_str().unwrap().into(),
        flags: 0,
    };
    c_data_rpc_as(&CONSOLE, gen::METHOD_JOB_COPY, &body.to_bytes().unwrap()).0
}

/// Waits until every job in `ids` is listed and finished.
async fn all_finished(ids: &[[u8; 16]]) {
    for _ in 0..400 {
        let all = list_all(&CONSOLE);
        if ids
            .iter()
            .all(|id| all.iter().any(|e| e.job_id == *id && e.state != 0))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("jobs did not finish");
}

/// Final review: the 32-slot job table is shared by every owner. Finished jobs hold their slot
/// until they are released; the console page lists all 32 and, once it has read them finished,
/// releases only its own, which frees room for new jobs.
#[tokio::test(flavor = "multi_thread")]
async fn table_holds_32_jobs_and_seen_console_jobs_make_room() {
    let _serial = SERIAL.lock().await;
    let t = two("any-cap32", 0).await;
    let mut pc = Vec::new();
    let mut mine = Vec::new();
    for n in 0..32u8 {
        let id = [0x40 + n; 16];
        // An immediately finished job.run reply already delivers its result and makes
        // its slot evictable. Hold this operation until the initial reply is running.
        t.srv.knob("op_hold", 1);
        let mut st = gen::ERR_BUSY as i32;
        for _ in 0..200 {
            st = if n % 2 == 0 {
                run_fsck_now(&t.a, id).await as i32
            } else {
                run_as_console(id)
            };
            if st != gen::ERR_BUSY as i32 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        t.srv.knob("op_hold", 0);
        assert_eq!(st, gen::STATUS_OK as i32, "job {n}");
        all_finished(&[id]).await;
        if n % 2 == 0 {
            pc.push(id)
        } else {
            mine.push(id)
        }
    }
    let every: Vec<_> = pc.iter().chain(mine.iter()).copied().collect();
    all_finished(&every).await;
    // list_all fits all 32 in one reply, whoever owns them.
    let all = list_all(&CONSOLE);
    assert_eq!(all.len(), 32);
    for id in &mine {
        assert_eq!(c_jobs_owner_is(id, &CONSOLE), 1);
    }
    // Full, and nothing was delivered: a 33rd job is refused, for the page as for a PC.
    assert_eq!(run_as_console([0x70; 16]), gen::ERR_BUSY as i32);
    assert_eq!(run_fsck_now(&t.b, [0x71; 16]).await, gen::ERR_BUSY);
    // The page read them finished: its own are released (oldest first when room is needed) ...
    c_jobs_release_seen(&CONSOLE);
    assert_eq!(list_all(&CONSOLE).len(), 32, "the grace keeps them listed");
    assert_eq!(run_as_console([0x70; 16]), gen::STATUS_OK as i32);
    assert_eq!(run_fsck_now(&t.b, [0x71; 16]).await, gen::STATUS_OK);
    // ... and a PC's job never is.
    let all = list_all(&CONSOLE);
    assert_eq!(all.len(), 32);
    for id in &pc {
        assert!(
            all.iter().any(|e| e.job_id == *id),
            "a PC's job stays listed"
        );
    }
    drop(t.srv);
}

/// A finished copy or operation the console page started is released a grace after the page first
/// read it finished (not the 10-minute park age); a paired PC's finished jobs stay listed.
#[tokio::test(flavor = "multi_thread")]
async fn seen_console_jobs_leave_after_the_grace_pc_jobs_stay() {
    let _serial = SERIAL.lock().await;
    let t = two("any-seen", 0).await;
    write_tree(&t.d.join("usb/g"), 5, |_| 1024);
    let (my_op, my_copy, pc_op, pc_copy) = ([0x81; 16], [0x82; 16], [0x83; 16], [0x84; 16]);
    assert_eq!(run_as_console(my_op), gen::STATUS_OK as i32);
    assert_eq!(
        copy_as_console(my_copy, &t.d.join("usb/g"), &t.d.join("data/mine")),
        gen::STATUS_OK as i32
    );
    assert_eq!(run_fsck_now(&t.a, pc_op).await, gen::STATUS_OK);
    assert_eq!(
        copy(&t.a, pc_copy, &t.d.join("usb/g"), &t.d.join("data/pc")).await,
        gen::STATUS_OK
    );
    all_finished(&[my_op, my_copy, pc_op, pc_copy]).await;
    assert_eq!(c_jobs_owner_is(&my_copy, &CONSOLE), 1);
    c_jobs_release_seen(&CONSOLE);
    let started = std::time::Instant::now();
    let listed = |id: [u8; 16]| list_all(&CONSOLE).iter().any(|e| e.job_id == id);
    assert!(
        listed(my_op) && listed(my_copy),
        "still listed during the grace"
    );
    // The page reads /api/jobs about once a second.
    let mut gone = false;
    while started.elapsed() < Duration::from_secs(20) {
        c_jobs_release_seen(&CONSOLE);
        if !listed(my_op) && !listed(my_copy) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        gone,
        "the page's finished jobs are released after the grace"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "not before the grace: {:?}",
        started.elapsed()
    );
    assert!(listed(pc_op), "a PC's finished operation stays");
    assert!(listed(pc_copy), "a PC's finished copy stays");
    // The PC still reads its own results.
    assert_eq!(
        code(&t.a, gen::METHOD_JOB_STATUS, pc_copy).await,
        gen::STATUS_OK
    );
    drop(t.srv);
}

async fn run_fsck_now(s: &Session, job: [u8; 16]) -> u16 {
    s.rpc(gen::METHOD_JOB_RUN, &fsck_body(job))
        .await
        .unwrap()
        .status
}
