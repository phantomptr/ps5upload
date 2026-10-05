//! P3 Task 5: long operations over the AVA1 management transport (`job.run`, polled), against
//! the Rust AVA1 server on 127.0.0.1 with a scripted console that keeps job state.
//! The C side of the same protocol is tested in `ava1-ctest/tests/job_run.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::gen::{self, JobRef, JobRun, Status};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::mgmt::AvaTransport;
use ps5upload_ava1::mgmt_job::{op_id_of, op_job_id};
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt::{self, ops, JobCall, MgmtError, MgmtTransport};

const T: Duration = Duration::from_secs(10);

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-mgmtjob-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ok(body: Vec<u8>) -> RpcReply {
    RpcReply {
        status: gen::STATUS_OK,
        body,
    }
}

fn err(status: u16, cause: &str) -> RpcReply {
    RpcReply {
        status,
        body: cause.as_bytes().to_vec(),
    }
}

/// What the scripted console does with a job.
#[derive(Clone, Default)]
struct Script {
    /// The status poll that reports the end (1-based).
    finish_at: u32,
    result: Vec<u8>,
    /// `Some((code, cause))`: the job ends in failure.
    fail: Option<(u16, &'static str)>,
    /// The status poll that answers `ERR_UNKNOWN_JOB` once (a console that forgot the job).
    forget_at: Option<u32>,
    /// `job.run` answers this status instead (busy, unknown method).
    run_refused: Option<u16>,
    /// `job.run` takes this long to answer (the job is not listed until it does).
    run_delay_ms: u64,
    /// The console does not advertise CAP_MGMT (an older helper): the transport sends nothing.
    no_cap: bool,
}

#[derive(Default)]
struct Job {
    op: u8,
    args: Vec<u8>,
    polls: u32,
    cancelled: bool,
}

#[derive(Default)]
struct Console {
    script: Script,
    jobs: HashMap<[u8; 16], Job>,
    runs: Vec<(u8, Vec<u8>)>,
    cancels: usize,
    /// The scripted forgetting happened (it happens once per console).
    forgot: bool,
}

fn status(job_id: [u8; 16], state: u8, polls: u32, current: Option<&str>) -> Status {
    Status {
        job_id,
        files_done: polls * 10,
        files_total: 100,
        bytes_received: polls as u64 * 1000,
        bytes_durable: polls as u64 * 1000,
        bytes_total: 100_000,
        bottleneck: 0,
        workers: 0,
        lanes: 0,
        sequential: 0,
        current: current.map(str::to_string),
        state: Some(state),
        result: None,
        code: None,
        unswept: None,
    }
}

fn handle(c: &Mutex<Console>, method: u16, body: &[u8]) -> RpcReply {
    if method == gen::METHOD_JOB_RUN {
        let delay = c.lock().unwrap().script.run_delay_ms;
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay)); // not listed until it answers
        }
    }
    let mut c = c.lock().unwrap();
    match method {
        gen::METHOD_JOB_RUN => {
            let r = JobRun::decode(body).unwrap();
            if let Some(s) = c.script.run_refused {
                return err(s, "refused");
            }

            c.runs.push((r.op, r.args.clone()));
            let j = c.jobs.entry(r.job_id).or_insert_with(|| Job {
                op: r.op,
                args: r.args.clone(),
                ..Job::default()
            });
            assert_eq!((j.op, &j.args), (r.op, &r.args), "idempotent run");
            ok(status(r.job_id, 0, 0, None).to_bytes().unwrap())
        }
        gen::METHOD_JOB_STATUS => {
            let id = JobRef::decode(body).unwrap().job_id;
            let script = c.script.clone();
            let Some(j) = c.jobs.get_mut(&id) else {
                return err(gen::ERR_UNKNOWN_JOB, "");
            };
            j.polls += 1;
            let polls = j.polls;
            if script.forget_at == Some(polls) && !c.forgot {
                c.forgot = true;
                c.jobs.remove(&id);
                return err(gen::ERR_UNKNOWN_JOB, "");
            }
            let j = c.jobs.get_mut(&id).unwrap();
            if j.cancelled {
                let mut s = status(id, 2, polls, Some("fs_delete_cancelled"));
                s.code = Some(gen::ERR_CANCELLED);
                return ok(s.to_bytes().unwrap());
            }
            if script.finish_at != 0 && polls >= script.finish_at {
                return ok(match script.fail {
                    Some((code, cause)) => {
                        let mut s = status(id, 2, polls, Some(cause));
                        s.code = Some(code);
                        s.to_bytes().unwrap()
                    }
                    None => {
                        let mut s = status(id, 1, polls, None);
                        s.result = Some(script.result.clone());
                        s.to_bytes().unwrap()
                    }
                });
            }
            ok(status(id, 0, polls, Some("deleting")).to_bytes().unwrap())
        }
        gen::METHOD_JOB_CANCEL => {
            let id = JobRef::decode(body).unwrap().job_id;
            c.cancels += 1;
            match c.jobs.get_mut(&id) {
                Some(j) => {
                    j.cancelled = true;
                    ok(vec![])
                }
                None => err(gen::ERR_UNKNOWN_JOB, ""),
            }
        }
        other => panic!("unexpected method {other}"),
    }
}

async fn console(
    tag: &str,
    script: Script,
) -> (
    Arc<AvaTransport>,
    &'static Pool,
    String,
    Arc<Mutex<Console>>,
) {
    let base = temp(tag);
    let ava = base.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let no_cap = script.no_cap;
    let state = Arc::new(Mutex::new(Console {
        script,
        ..Console::default()
    }));
    let s2 = state.clone();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        Box::new(move |m, b| handle(&s2, m, b)),
    )
    .with_jobs(Arc::new(FolderHost {
        root: base.join("share"),
        jobs_dir: base.join("jobs"),
    }));
    let ctx = if no_cap { ctx } else { ctx.with_mgmt() };
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
    let t = AvaTransport::with_pool(pool).with_busy_delays([Duration::from_millis(5); 3]);
    (Arc::new(t), pool, format!("{tag}-console"), state)
}

async fn run_op(
    t: &Arc<AvaTransport>,
    addr: &str,
    op: mgmt::JobOp,
    body: &str,
    op_id: u64,
    subject: &str,
    deadline: Duration,
) -> anyhow::Result<Option<Vec<u8>>> {
    let (t, addr, body, subject) = (
        t.clone(),
        addr.to_string(),
        body.to_string(),
        subject.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        t.run_job(
            &addr,
            op,
            op.label,
            body.as_bytes(),
            &JobCall {
                op_id,
                subject: &subject,
                deadline,
            },
        )
    })
    .await
    .unwrap()
}

#[test]
fn op_ids_are_the_low_eight_bytes_of_the_job_id() {
    let id = op_job_id(77, [5; 8]);
    assert_eq!(op_id_of(&id), 77);
    assert_eq!(id[8..], 77u64.to_le_bytes());
}

#[test]
fn the_operation_table_matches_the_schema() {
    let want = [
        (ops::DELETE, gen::JOB_OP_DELETE),
        (ops::CHMOD_R, gen::JOB_OP_CHMOD_R),
        (ops::HASH, gen::JOB_OP_HASH),
        (ops::CRC32, gen::JOB_OP_CRC32),
        (ops::FSCK, gen::JOB_OP_FSCK),
        (ops::BACKUP_SNAPSHOT, gen::JOB_OP_BACKUP_SNAPSHOT),
        (ops::BACKUP_RESTORE, gen::JOB_OP_BACKUP_RESTORE),
        (ops::CLEANUP, gen::JOB_OP_CLEANUP),
        (ops::SDK_SCAN, gen::JOB_OP_SDK_SCAN),
    ];
    assert_eq!(want.len(), ops::ALL.len(), "every op is pinned here");
    for (op, id) in want {
        assert_eq!(op.id, id, "{}", op.label);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_is_run_polled_and_its_result_returned() {
    let (t, _p, c, st) = console(
        "ok",
        Script {
            finish_at: 3,
            result: br#"{"crc32":7,"size":9}"#.to_vec(),
            ..Script::default()
        },
    )
    .await;
    let started = Instant::now();
    let r = run_op(&t, &c, ops::CRC32, r#"{"path":"/data/x"}"#, 0, "", T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, br#"{"crc32":7,"size":9}"#);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "polled, not blocked: {:?}",
        started.elapsed()
    );
    let st = st.lock().unwrap();
    assert_eq!(
        st.runs,
        vec![(gen::JOB_OP_CRC32, br#"{"path":"/data/x"}"#.to_vec())]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_is_visible_by_op_id_while_it_runs_and_gone_after() {
    let (t, _p, c, _st) = console(
        "progress",
        Script {
            finish_at: 8,
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let runner = tokio::spawn(async move {
        run_op(
            &t2,
            &c2,
            ops::DELETE,
            r#"{"path":"/data/g"}"#,
            9_001,
            "/data/g",
            T,
        )
        .await
    });
    let mut seen = Vec::new();
    for _ in 0..40 {
        let (t, c) = (t.clone(), c.clone());
        let p = tokio::task::spawn_blocking(move || t.job_progress(&c, 9_001))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(p) = p {
            assert_eq!(
                (p.kind.as_str(), p.subject.as_str()),
                ("fs_delete", "/data/g")
            );
            assert_eq!(p.bytes_total, 100_000);
            seen.push(p.files_done);
        }
        if runner.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(runner.await.unwrap().unwrap().is_some());
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]) && seen.last().unwrap() > seen.first().unwrap(),
        "{seen:?}"
    );
    // After the call ended nothing is registered: `found: false`.
    let (t2, c2) = (t.clone(), c.clone());
    let gone = tokio::task::spawn_blocking(move || t2.job_progress(&c2, 9_001))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gone, Some(None));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_by_op_id_ends_the_wait_with_the_consoles_cancel_token() {
    let (t, _p, c, st) = console(
        "cancel",
        Script {
            finish_at: 1_000,
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let runner = tokio::spawn(async move {
        run_op(
            &t2,
            &c2,
            ops::DELETE,
            r#"{"path":"/data/g"}"#,
            9_002,
            "/data/g",
            T,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (t2, c2) = (t.clone(), c.clone());
    let found = tokio::task::spawn_blocking(move || t2.job_cancel(&c2, 9_002))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found, Some(true));
    let e = runner.await.unwrap().unwrap_err();
    let m = e.downcast_ref::<MgmtError>().expect("a console refusal");
    assert_eq!(
        (m.status, m.cause.as_str()),
        (gen::ERR_CANCELLED, "fs_delete_cancelled")
    );
    assert_eq!(
        e.to_string(),
        "payload rejected FS_DELETE: fs_delete_cancelled"
    );
    assert_eq!(st.lock().unwrap().cancels, 1);
    // An op id this process does not run: served, not found.
    let (t2, c2) = (t.clone(), c.clone());
    let none = tokio::task::spawn_blocking(move || t2.job_cancel(&c2, 424_242))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(none, Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_job_is_the_payload_rejected_error_with_the_consoles_status_and_cause() {
    let (t, _p, c, _st) = console(
        "fail",
        Script {
            finish_at: 2,
            fail: Some((gen::ERR_IO, "fs_delete_failed")),
            ..Script::default()
        },
    )
    .await;
    let e = run_op(&t, &c, ops::DELETE, r#"{"path":"/data/g"}"#, 0, "", T)
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected FS_DELETE: fs_delete_failed"
    );
    assert_eq!(e.downcast_ref::<MgmtError>().unwrap().status, gen::ERR_IO);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_session_does_not_lose_the_job() {
    let (t, pool, c, st) = console(
        "reconnect",
        Script {
            finish_at: 6,
            result: b"{}".to_vec(),
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let runner = tokio::spawn(async move {
        run_op(&t2, &c2, ops::HASH, r#"{"path":"/data/x"}"#, 0, "", T).await
    });
    tokio::time::sleep(Duration::from_millis(700)).await;
    pool.forget(&c).await; // the session is dropped mid-run; the next poll connects again
    let r = runner.await.unwrap().unwrap().unwrap();
    assert_eq!(r, b"{}");
    assert_eq!(
        st.lock().unwrap().runs.len(),
        1,
        "one run: the poll found the job again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_the_console_forgot_is_started_again_except_a_snapshot() {
    let script = Script {
        finish_at: 3,
        forget_at: Some(1),
        result: b"{\"ok\":true}".to_vec(),
        ..Script::default()
    };
    let (t, _p, c, st) = console("forget", script.clone()).await;
    let r = run_op(
        &t,
        &c,
        ops::CHMOD_R,
        r#"{"path":"/data/x","mode":"0777"}"#,
        0,
        "",
        T,
    )
    .await
    .unwrap();
    assert!(r.is_some());
    assert_eq!(
        st.lock().unwrap().runs.len(),
        2,
        "the run was repeated once"
    );
    // A snapshot would be taken twice: it is an error instead.
    let (t, _p, c, st) = console("forget-snap", script).await;
    let e = run_op(
        &t,
        &c,
        ops::BACKUP_SNAPSHOT,
        r#"{"tag":"t","path":"/data/x"}"#,
        0,
        "",
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "payload rejected BACKUP_SNAPSHOT: job_lost");
    assert_eq!(st.lock().unwrap().runs.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_ends_at_the_callers_deadline_and_says_the_job_may_still_run() {
    let (t, _p, c, st) = console("deadline", Script::default()).await;
    let started = Instant::now();
    let e = run_op(
        &t,
        &c,
        ops::FSCK,
        r#"{"device":"/dev/md1"}"#,
        0,
        "",
        Duration::from_millis(1300),
    )
    .await
    .unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("no result after") && msg.contains("may still be running"),
        "{msg}"
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    // Giving up cancels the job on the console: nobody is waiting for it any more.
    assert_eq!(st.lock().unwrap().cancels, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_progress_query_before_the_job_is_listed_waits_instead_of_reading_zero() {
    let (t, _p, c, _st) = console(
        "unlisted",
        Script {
            finish_at: 1_000,
            run_delay_ms: 500,
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let runner = tokio::spawn(async move {
        run_op(
            &t2,
            &c2,
            ops::DELETE,
            "{}",
            9_300,
            "/data/g",
            Duration::from_secs(3),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // the run call is in flight, unanswered
    let started = Instant::now();
    let (t3, c3) = (t.clone(), c.clone());
    let p = tokio::task::spawn_blocking(move || t3.job_progress(&c3, 9_300))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .expect("the op is registered");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "waited for the listing: {:?}",
        started.elapsed()
    );
    assert_eq!(
        p.files_total, 100,
        "the console's own numbers, not a zeroed placeholder"
    );
    assert_eq!(
        (p.kind.as_str(), p.subject.as_str()),
        ("fs_delete", "/data/g")
    );
    let _ = runner.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_helper_is_helper_not_ava1_and_busy_is_a_refusal() {
    let (t, _p, c, _st) = console(
        "old",
        Script {
            no_cap: true,
            ..Script::default()
        },
    )
    .await;
    let e = run_op(&t, &c, ops::DELETE, r#"{"path":"/data/x"}"#, 0, "", T)
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("helper_not_ava1"),
        "no CAP_MGMT: the error the person can act on, nothing sent: {e}"
    );
    // a helper that advertises CAP_MGMT but predates job.run answers ERR_UNKNOWN_METHOD: the
    // transport reports it as not served, which the core turns into helper_not_ava1
    let (t, _p, c, _st) = console(
        "old-job-run",
        Script {
            run_refused: Some(gen::ERR_UNKNOWN_METHOD),
            ..Script::default()
        },
    )
    .await;
    assert!(
        run_op(&t, &c, ops::DELETE, r#"{"path":"/data/x"}"#, 0, "", T)
            .await
            .unwrap()
            .is_none()
    );
    let (t, _p, c, _st) = console(
        "busy",
        Script {
            run_refused: Some(gen::ERR_BUSY),
            ..Script::default()
        },
    )
    .await;
    let e = run_op(&t, &c, ops::DELETE, r#"{"path":"/data/x"}"#, 0, "", T)
        .await
        .unwrap_err();
    assert_eq!(e.downcast_ref::<MgmtError>().unwrap().status, gen::ERR_BUSY);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_op_id_cannot_run_twice_at_once() {
    let (t, _p, c, _st) = console(
        "dup",
        Script {
            finish_at: 5,
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let first =
        tokio::spawn(async move { run_op(&t2, &c2, ops::DELETE, "{}", 9_100, "", T).await });
    tokio::time::sleep(Duration::from_millis(250)).await;
    let e = run_op(&t, &c, ops::DELETE, "{}", 9_100, "", T)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("already running"), "{e}");
    assert!(first.await.unwrap().is_ok());
    // And once it ended the id is free again.
    assert!(run_op(&t, &c, ops::DELETE, "{}", 9_100, "", T)
        .await
        .is_ok());
}

// ---- through the core API the engine calls ----

#[tokio::test(flavor = "multi_thread")]
async fn fs_delete_over_ava1_is_a_job_and_a_cancel_is_the_word_cancelled() {
    let (t, _p, c, st) = console(
        "core-del",
        Script {
            finish_at: 1_000,
            ..Script::default()
        },
    )
    .await;
    let (t2, c2) = (t.clone(), c.clone());
    let deleter = tokio::task::spawn_blocking(move || {
        let _g = mgmt::scoped_transport(t2);
        ps5upload_core::fs_ops::fs_delete_with_op_id(&c2, "/data/g", 9_200, Some(T))
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (t3, c3) = (t.clone(), c.clone());
    let snap = tokio::task::spawn_blocking(move || {
        let _g = mgmt::scoped_transport(t3);
        let s = ps5upload_core::fs_ops::fs_op_status(&c3, 9_200).unwrap();
        let cancelled = ps5upload_core::fs_ops::fs_op_cancel(&c3, 9_200).unwrap();
        (s, cancelled)
    })
    .await
    .unwrap();
    assert!(snap.0.found);
    assert_eq!(
        (snap.0.kind.as_str(), snap.0.from.as_str()),
        ("fs_delete", "/data/g")
    );
    assert_eq!(snap.0.total_bytes, 100_000);
    assert!(snap.0.bytes_copied > 0);
    assert!(snap.1);
    let e = deleter.await.unwrap().unwrap_err();
    assert_eq!(e.to_string(), "cancelled");
    assert_eq!(st.lock().unwrap().runs[0].0, gen::JOB_OP_DELETE);
}

#[tokio::test(flavor = "multi_thread")]
async fn recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes() {
    let (t, _p, c, st) = console(
        "core-ops",
        Script {
            finish_at: 1,
            result: Vec::new(),
            ..Script::default()
        },
    )
    .await;
    let set = |r: &[u8]| st.lock().unwrap().script.result = r.to_vec();
    let c2 = c.clone();
    let t2 = t.clone();
    // Each call sets the reply the console returns, then runs the core function.
    set(b"");
    tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::fs_ops::fs_chmod(&c, "/data/g", "0777", true).unwrap();
        }
    })
    .await
    .unwrap();
    set(br#"{"path":"/data/f","size":12,"hash":"ab"}"#);
    let h = tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::fs_ops::fs_hash(&c, "/data/f").unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!((h.size, h.hash.as_str()), (12, "ab"));
    set(br#"{"crc32":5,"size":8}"#);
    let crc = tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::diagnostics::crc32_file(&c, "/data/f").unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!((crc.crc32, crc.size), (Some(5), Some(8)));
    // fsck keeps its ok:false body as an error the caller reads.
    set(br#"{"ok":false,"code":3,"device":"/dev/md1","repair":false}"#);
    let e = tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::diagnostics::ufs_fsck(&c, "/dev/md1", false).unwrap_err()
        }
    })
    .await
    .unwrap();
    assert!(e.to_string().starts_with("UFS_FSCK failed"), "{e}");
    set(br#"{"ok":true,"tag":"t","timestamp":1,"files":2,"bytes":3,"err":""}"#);
    let s = tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::backup::backup_snapshot(&c, "t", "/data/x").unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(s.files, 2);
    set(br#"{"ok":false,"tag":"t","restored":0,"err":"snapshot not found"}"#);
    let e = tokio::task::spawn_blocking({
        let (t, c) = (t2.clone(), c2.clone());
        move || {
            let _g = mgmt::scoped_transport(t);
            ps5upload_core::backup::backup_restore(&c, "t", 1).unwrap_err()
        }
    })
    .await
    .unwrap();
    assert!(e.to_string().contains("snapshot not found"), "{e}");
    let ops_run: Vec<u8> = st.lock().unwrap().runs.iter().map(|r| r.0).collect();
    assert_eq!(
        ops_run,
        vec![
            gen::JOB_OP_CHMOD_R,
            gen::JOB_OP_HASH,
            gen::JOB_OP_CRC32,
            gen::JOB_OP_FSCK,
            gen::JOB_OP_BACKUP_SNAPSHOT,
            gen::JOB_OP_BACKUP_RESTORE
        ]
    );
}

/// A refused crc32 keeps the shape the handler answered: a body with `err`.
#[tokio::test(flavor = "multi_thread")]
async fn a_crc32_the_console_could_not_run_is_an_err_field_not_an_error() {
    let (t, _p, c, _st) = console(
        "crc-err",
        Script {
            finish_at: 1,
            fail: Some((gen::ERR_IO, "crc32_open_failed")),
            ..Script::default()
        },
    )
    .await;
    let r = tokio::task::spawn_blocking(move || {
        let _g = mgmt::scoped_transport(t);
        ps5upload_core::diagnostics::crc32_file(&c, "/data/missing").unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        (r.crc32, r.err.as_deref()),
        (None, Some("crc32_open_failed"))
    );
}
