//! The AVA1 management transport end to end, against the Rust AVA1 server on 127.0.0.1
//! with a scripted RPC handler. Public API only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::gen::{self, MgmtText};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::mgmt::{AvaTransport, GENERAL};
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt::{m, MgmtError, MgmtTransport};

const T: Duration = Duration::from_secs(10);

/// `job.cancel` (18) has no legacy-JSON method in the table yet (Task 5 converts the job
/// calls); the gate test only needs its number.
const JOB_CANCEL: ps5upload_core::mgmt::Method = ps5upload_core::mgmt::Method {
    id: gen::METHOD_JOB_CANCEL,
    label: "FS_OP_CANCEL",
};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-mgmt-{tag}-{}", std::process::id()));
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

fn text(s: &str) -> RpcReply {
    ok(MgmtText {
        body: s.as_bytes().to_vec(),
        more: None,
    }
    .to_bytes()
    .unwrap())
}

fn text_of(req: &[u8]) -> String {
    String::from_utf8(MgmtText::decode(req).unwrap().body).unwrap()
}

/// A loopback console running `handler` that trusts the transport's identity, and the
/// transport over its own pool.
async fn console(tag: &str, handler: RpcHandler) -> (Arc<AvaTransport>, &'static Pool, String) {
    let base = temp(tag);
    let ava = base.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, handler)
        .with_jobs(Arc::new(FolderHost {
            root: base.join("share"),
            jobs_dir: base.join("jobs"),
        }))
        .with_mgmt();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
    let t = AvaTransport::with_pool(pool).with_busy_delays([Duration::from_millis(5); 3]);
    // The console string is only a key: the pool resolves it to the loopback address.
    (Arc::new(t), pool, format!("{tag}-console"))
}

/// `AvaTransport::call` is blocking (it is called from `spawn_blocking` in the engine).
async fn call(
    t: &Arc<AvaTransport>,
    addr: &str,
    method: ps5upload_core::mgmt::Method,
    label: &str,
    body: &[u8],
    timeout: Duration,
) -> anyhow::Result<Option<Vec<u8>>> {
    let (t, addr, label, body) = (
        t.clone(),
        addr.to_string(),
        label.to_string(),
        body.to_vec(),
    );
    tokio::task::spawn_blocking(move || t.call(&addr, method, &label, &body, timeout))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_text_method_carries_the_legacy_body_unchanged_both_ways() {
    let (t, _p, c) = console(
        "text",
        Box::new(|method, body| {
            assert_eq!(method, gen::METHOD_HW_INFO);
            text(&format!("model=PS5\necho={}", text_of(body)))
        }),
    )
    .await;
    let r = call(&t, &c, m::HW_INFO, "HW_INFO", b"x=1", T)
        .await
        .unwrap();
    assert_eq!(r.unwrap(), b"model=PS5\necho=x=1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_typed_node_status_rebuilds_the_legacy_json_with_a_bool_and_replaced() {
    let (t, _p, c) = console(
        "status",
        Box::new(|method, body| {
            assert_eq!(method, gen::METHOD_NODE_STATUS);
            assert!(body.is_empty());
            ok(gen::NodeStatus {
                version: "5.42.0".into(),
                ps5_kernel: "FreeBSD 13".into(),
                instance_id: 42,
                started_at_unix: 1_700_000_000,
                command_count: 9,
                startup_reason: 1,
                ucred_elevated: 1,
                max_transfer_streams: 4,
                fan_threshold: 70,
                fan_reapply_sec: 30,
                prior_instance: Some("replaced".into()),
            }
            .to_bytes()
            .unwrap())
        }),
    )
    .await;
    let r = call(&t, &c, m::NODE_STATUS, "STATUS", b"", T)
        .await
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(v["ucred_elevated"], serde_json::Value::Bool(true));
    assert_eq!(v["prior_instance"], "replaced");
    assert_eq!(v["version"], "5.42.0");
    assert_eq!(v["instance_id"], 42);
    assert_eq!(v["max_transfer_streams"], 4);
    assert_eq!(v["fan_threshold"], 70);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_typed_list_is_sent_typed_and_comes_back_as_the_legacy_listing() {
    let (t, _p, c) = console(
        "list",
        Box::new(|method, body| {
            assert_eq!(method, gen::METHOD_FS_LIST);
            let q = gen::FsList::decode(body).unwrap();
            assert_eq!((q.path.as_str(), q.offset, q.limit), ("/data", 5, 100));
            ok(gen::FsListResult {
                entries: vec![
                    gen::FsEntry {
                        name: "a.bin".into(),
                        kind: gen::ENTRY_FILE,
                        size: 7,
                        mtime: Some(99),
                        mode: None,
                    },
                    gen::FsEntry {
                        name: "d".into(),
                        kind: gen::ENTRY_DIR,
                        size: 0,
                        mtime: None,
                        mode: None,
                    },
                ],
                total_scanned: 12,
                more: 1,
            }
            .to_bytes()
            .unwrap())
        }),
    )
    .await;
    let body = br#"{"path":"/data","offset":5,"limit":100}"#;
    let r = call(&t, &c, m::FS_LIST, "FS_LIST_DIR(/data)", body, T)
        .await
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(v["path"], "/data");
    assert_eq!(v["returned"], 2);
    assert_eq!(v["total_scanned"], 12);
    assert_eq!(v["truncated"], true);
    assert_eq!(v["entries"][0]["name"], "a.bin");
    assert_eq!(v["entries"][0]["kind"], "file");
    assert_eq!(v["entries"][0]["mtime"], 99);
    assert_eq!(v["entries"][1]["kind"], "dir");
    assert_eq!(v["entries"][1]["mtime"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cross_device_rename_keeps_the_text_callers_match_on() {
    let (t, _p, c) = console(
        "xdev",
        Box::new(|method, body| {
            assert_eq!(method, gen::METHOD_FS_RENAME);
            let q = gen::FsRename::decode(body).unwrap();
            assert_eq!((q.from.as_str(), q.to.as_str()), ("/a", "/b"));
            err(gen::ERR_CROSS_DEVICE, "fs_move_cross_mount")
        }),
    )
    .await;
    let e = call(
        &t,
        &c,
        m::FS_RENAME,
        "FS_MOVE",
        br#"{"from":"/a","to":"/b"}"#,
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected FS_MOVE: fs_move_cross_mount"
    );
    let me = e.downcast_ref::<MgmtError>().unwrap();
    assert_eq!(me.status, gen::ERR_CROSS_DEVICE);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_without_a_cause_gets_a_readable_legacy_token() {
    let (t, _p, c) = console("nocause", Box::new(|_, _| err(gen::ERR_EXISTS, ""))).await;
    let e = call(&t, &c, m::FS_MKDIR, "FS_MKDIR", br#"{"path":"/x"}"#, T)
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "payload rejected FS_MKDIR: exists");
}

#[tokio::test(flavor = "multi_thread")]
async fn mkdir_chmod_and_stat_send_typed_bodies() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let s2 = seen.clone();
    let (t, _p, c) = console(
        "misc",
        Box::new(move |method, body| match method {
            gen::METHOD_FS_MKDIR => {
                let q = gen::FsMkdir::decode(body).unwrap();
                s2.lock()
                    .unwrap()
                    .push(format!("mkdir {} {:o} {}", q.path, q.mode, q.parents));
                ok(vec![])
            }
            gen::METHOD_FS_CHMOD => {
                let q = gen::FsChmod::decode(body).unwrap();
                s2.lock()
                    .unwrap()
                    .push(format!("chmod {} {:o}", q.path, q.mode));
                ok(vec![])
            }
            gen::METHOD_FS_STAT => {
                let q = gen::FsPath::decode(body).unwrap();
                s2.lock().unwrap().push(format!("stat {}", q.path));
                ok(gen::FsStat {
                    kind: gen::ENTRY_LINK,
                    size: 5,
                    mtime: 6,
                    mode: 0o644,
                    dev: 7,
                }
                .to_bytes()
                .unwrap())
            }
            gen::METHOD_FS_FREESPACE => {
                let q = gen::FsPath::decode(body).unwrap();
                s2.lock().unwrap().push(format!("freespace {}", q.path));
                ok(gen::FsFreeSpace {
                    usable: 900,
                    free: 1000,
                    total: 5000,
                    reserve: 100,
                    dev: 9,
                }
                .to_bytes()
                .unwrap())
            }
            _ => err(gen::ERR_UNKNOWN_METHOD, ""),
        }),
    )
    .await;
    let r = call(
        &t,
        &c,
        m::FS_FREESPACE,
        "FS_FREESPACE",
        br#"{"path":"/data/x"}"#,
        T,
    )
    .await
    .unwrap()
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(
        (
            v["usable_bytes"].as_u64(),
            v["free_bytes"].as_u64(),
            v["total_bytes"].as_u64(),
            v["reserve_bytes"].as_u64(),
            v["dev"].as_u64()
        ),
        (Some(900), Some(1000), Some(5000), Some(100), Some(9))
    );
    call(&t, &c, m::FS_MKDIR, "FS_MKDIR", br#"{"path":"/a/b"}"#, T)
        .await
        .unwrap();
    call(
        &t,
        &c,
        m::FS_CHMOD,
        "FS_CHMOD",
        br#"{"path":"/a","mode":"0755","recursive":0}"#,
        T,
    )
    .await
    .unwrap();
    let r = call(&t, &c, m::FS_STAT, "FS_STAT", br#"{"path":"/a"}"#, T)
        .await
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(
        (v["kind"].as_str(), v["size"].as_u64(), v["dev"].as_u64()),
        (Some("link"), Some(5), Some(7))
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "freespace /data/x",
            "mkdir /a/b 777 1",
            "chmod /a 755",
            "stat /a"
        ]
    );
}

/// A recursive chmod is a `job.run` CHMOD_R op (progress, cancel, no socket held for minutes);
/// the legacy body is the op's arguments, and the caller gets the empty reply it always did.
#[tokio::test(flavor = "multi_thread")]
async fn a_recursive_chmod_runs_as_a_job() {
    let seen = Arc::new(Mutex::new(Vec::<(u16, Vec<u8>)>::new()));
    let s2 = seen.clone();
    let (t, _p, c) = console(
        "rchmod",
        Box::new(move |method, body| {
            s2.lock().unwrap().push((method, body.to_vec()));
            let id = match method {
                gen::METHOD_JOB_RUN => gen::JobRun::decode(body).unwrap().job_id,
                gen::METHOD_JOB_STATUS => gen::JobRef::decode(body).unwrap().job_id,
                _ => return err(gen::ERR_INTERNAL, "unexpected"),
            };
            ok(job_status(id, 1).to_bytes().unwrap())
        }),
    )
    .await;
    let body = br#"{"path":"/a","mode":"0777","recursive":1}"#;
    let r = call(&t, &c, m::FS_CHMOD, "FS_CHMOD", body, T)
        .await
        .unwrap();
    assert_eq!(r, Some(Vec::new()));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, gen::METHOD_JOB_RUN);
    let run = gen::JobRun::decode(&seen[0].1).unwrap();
    assert_eq!(
        (run.op, run.args.as_slice()),
        (gen::JOB_OP_CHMOD_R, &body[..])
    );
}

fn file_bytes(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8)
        .collect()
}

/// A scripted file server: serves `file` at the asked offset, at most `cap` bytes per call.
fn read_server(file: Vec<u8>, cap: usize, calls: Arc<Mutex<Vec<(u64, u32, u32)>>>) -> RpcHandler {
    Box::new(move |method, body| {
        assert_eq!(method, gen::METHOD_FS_READ);
        let q = gen::FsRead::decode(body).unwrap();
        calls.lock().unwrap().push((q.offset, q.len, q.flags));
        assert!(
            q.len <= gen::FS_READ_MAX,
            "one call never asks past the cap"
        );
        let lo = (q.offset as usize).min(file.len());
        let hi = (lo + (q.len as usize).min(cap)).min(file.len());
        ok(gen::FsReadResult {
            data: file[lo..hi].to_vec(),
            eof: (hi == file.len()) as u8,
        }
        .to_bytes()
        .unwrap())
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_read_loops_until_the_bytes_asked_for_arrive() {
    let file = file_bytes(1_000_000);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (t, _p, c) = console("read", read_server(file.clone(), usize::MAX, calls.clone())).await;
    let body = br#"{"path":"/big","offset":1000,"limit":600000,"unsafe":true}"#;
    let r = call(&t, &c, m::FS_READ, "FS_READ(/big)", body, T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, &file[1000..601_000]);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3, "600000 bytes at 262128 per call");
    assert_eq!(calls[0], (1000, gen::FS_READ_MAX, gen::FSR_UNSAFE));
    assert_eq!(calls[1].0, 1000 + gen::FS_READ_MAX as u64);
    assert_eq!(calls[2].1, 600_000 - 2 * gen::FS_READ_MAX);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_stops_at_eof_and_continues_after_a_short_read() {
    let file = file_bytes(250_000);
    let calls = Arc::new(Mutex::new(Vec::new()));
    // The node chooses 100 000-byte reads (eof 0 with fewer bytes than asked).
    let (t, _p, c) = console("short", read_server(file.clone(), 100_000, calls.clone())).await;
    let body = br#"{"path":"/f","offset":0,"limit":2000000,"unsafe":false}"#;
    let r = call(&t, &c, m::FS_READ, "FS_READ(/f)", body, T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, file, "eof ended the loop at the file's end");
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_never_asks_past_the_per_call_ceiling() {
    let file = file_bytes(3 * 1024 * 1024);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (t, _p, c) = console("ceiling", read_server(file.clone(), usize::MAX, calls)).await;
    let body = br#"{"path":"/f","offset":0,"limit":99999999}"#;
    let r = call(&t, &c, m::FS_READ, "FS_READ(/f)", body, T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.len(), 2 * 1024 * 1024);
    assert_eq!(r, &file[..2 * 1024 * 1024]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_error_keeps_the_payload_cause() {
    let (t, _p, c) = console(
        "readerr",
        Box::new(|_, _| err(gen::ERR_PATH, "fs_read_path_not_allowed")),
    )
    .await;
    let e = call(
        &t,
        &c,
        m::FS_READ,
        "FS_READ(/x)",
        br#"{"path":"/x","limit":10}"#,
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected FS_READ(/x): fs_read_path_not_allowed"
    );
}

type Writes = Arc<Mutex<Vec<(String, u64, u32, usize)>>>;

fn write_server(log: Writes, fail_with: Option<(u16, &'static str)>) -> RpcHandler {
    Box::new(move |method, body| {
        assert_eq!(method, gen::METHOD_FS_WRITE);
        let q = gen::FsWrite::decode(body).unwrap();
        assert!(q.data.len() <= gen::FSW_CHUNK_MAX as usize);
        log.lock()
            .unwrap()
            .push((q.path, q.offset, q.flags, q.data.len()));
        match fail_with {
            Some((s, c)) => err(s, c),
            None => ok(vec![]),
        }
    })
}

fn write_body(path: &str, n: usize, mode: &str) -> Vec<u8> {
    use base64::Engine as _;
    serde_json::to_vec(&serde_json::json!({
        "path": path,
        "bytes": base64::engine::general_purpose::STANDARD.encode(file_bytes(n)),
        "mode": mode,
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_small_write_is_one_atomic_call() {
    let log: Writes = Arc::default();
    let (t, _p, c) = console("w1", write_server(log.clone(), None)).await;
    let r = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/data/a", 1000, "overwrite"),
        T,
    )
    .await
    .unwrap()
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(
        (v["ok"].as_bool(), v["size"].as_u64()),
        (Some(true), Some(1000))
    );
    assert_eq!(
        *log.lock().unwrap(),
        vec![("/data/a".to_string(), 0, gen::FSW_OVERWRITE, 1000)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_write_goes_in_chunks_at_their_offsets_and_commits_on_the_last() {
    let log: Writes = Arc::default();
    let (t, _p, c) = console("w3", write_server(log.clone(), None)).await;
    let n = 2 * gen::FSW_CHUNK_MAX as usize + 1234;
    let r = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/data/big", n, "create"),
        T,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&r).unwrap()["size"],
        n
    );
    let chunk = gen::FSW_CHUNK_MAX as u64;
    let base = gen::FSW_AT_OFFSET | gen::FSW_CREATE;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            ("/data/big".to_string(), 0, base, chunk as usize),
            ("/data/big".to_string(), chunk, base, chunk as usize),
            (
                "/data/big".to_string(),
                2 * chunk,
                base | gen::FSW_COMMIT,
                1234
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_chunk_stops_the_write_and_a_create_refusal_reads_as_the_legacy_body() {
    let log: Writes = Arc::default();
    let (t, _p, c) = console(
        "wfail",
        write_server(log.clone(), Some((gen::ERR_EXISTS, "exists"))),
    )
    .await;
    let n = 2 * gen::FSW_CHUNK_MAX as usize;
    // The refusal is an error in the seam ...
    let e = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/p", n, "create"),
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "payload rejected FS_WRITE_BYTES: exists");
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "no further chunk after a refusal"
    );
    // ... and `legacy_ok` (what `fs_write_bytes` uses) restores the old failure body.
    let body = ps5upload_core::mgmt::legacy_ok(Err(e)).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (v["ok"].as_bool(), v["err"].as_str()),
        (Some(false), Some("exists"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_over_the_per_call_ceiling_is_refused_like_before() {
    let (t, _p, c) = console("wbig", write_server(Arc::default(), None)).await;
    let e = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/p", 256 * 1024 + 1, "overwrite"),
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "payload rejected FS_WRITE_BYTES: too_large");
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_is_retried_and_then_succeeds() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let (t, _p, c) = console(
        "busy",
        Box::new(move |_, _| {
            if n2.fetch_add(1, Ordering::SeqCst) < 3 {
                err(gen::ERR_BUSY, "busy")
            } else {
                text("done")
            }
        }),
    )
    .await;
    let r = call(&t, &c, m::HW_INFO, "HW_INFO", b"", T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, b"done");
    assert_eq!(
        n.load(Ordering::SeqCst),
        4,
        "three retries after the first try"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_that_never_clears_surfaces_after_three_retries() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let (t, _p, c) = console(
        "busy2",
        Box::new(move |_, _| {
            n2.fetch_add(1, Ordering::SeqCst);
            err(gen::ERR_BUSY, "busy")
        }),
    )
    .await;
    let e = call(&t, &c, m::HW_INFO, "HW_INFO", b"", T)
        .await
        .unwrap_err();
    assert_eq!(e.downcast_ref::<MgmtError>().unwrap().status, gen::ERR_BUSY);
    assert_eq!(n.load(Ordering::SeqCst), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_session_serves_every_call() {
    let (t, p, c) = console("one", Box::new(|_, _| text("x"))).await;
    for _ in 0..5 {
        call(&t, &c, m::HW_INFO, "HW_INFO", b"", T).await.unwrap();
    }
    call(&t, &c, m::HW_TEMPS, "HW_TEMPS", b"", T).await.unwrap();
    assert_eq!(
        p.attempts(),
        1,
        "the management path never opens a second session"
    );
}

/// A console that does not advertise CAP_MGMT (an older AVA1 helper: transfers, no management
/// methods) is `helper_not_ava1` from its capability bits alone. Nothing is sent to it to find out.
#[tokio::test(flavor = "multi_thread")]
async fn a_console_without_cap_mgmt_is_helper_not_ava1_without_a_request() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let base = temp("old");
    let ava = base.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    // The data plane yes, management no: no `.with_mgmt()`.
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "old helper",
        peers,
        Box::new(move |_, _| {
            n2.fetch_add(1, Ordering::SeqCst);
            err(gen::ERR_UNKNOWN_METHOD, "")
        }),
    )
    .with_jobs(Arc::new(FolderHost {
        root: base.join("share"),
        jobs_dir: base.join("jobs"),
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
    let t = Arc::new(AvaTransport::with_pool(pool));
    let c = "old-console";
    for m in [m::HW_INFO, m::FS_LIST, m::NODE_STATUS] {
        let e = call(&t, c, m, "X", b"{}", T).await.unwrap_err();
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
    }
    assert_eq!(
        n.load(Ordering::SeqCst),
        0,
        "no management request reached it"
    );
    assert_eq!(
        pool.attempts(),
        1,
        "one session, reused for the three answers"
    );
}

/// A console that advertises CAP_MGMT is served over AVA1 even though it has no data plane.
#[tokio::test(flavor = "multi_thread")]
async fn cap_mgmt_alone_routes_management_over_ava1() {
    let base = temp("mgmtonly");
    let ava = base.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "mgmt only",
        peers,
        Box::new(|_, _| text("hi")),
    )
    .with_mgmt();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
    let t = Arc::new(AvaTransport::with_pool(pool));
    let r = call(&t, "mo-console", m::HW_INFO, "HW_INFO", b"", T)
        .await
        .unwrap();
    assert_eq!(r.unwrap(), b"hi");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_console_is_helper_not_ava1() {
    let base = temp("down");
    let pool: &'static Pool = Box::leak(Box::new(
        Pool::new(base.join("ava")).with_addr("127.0.0.1:1"),
    ));
    let t = Arc::new(AvaTransport::with_pool(pool));
    let e = call(&t, "down-console", m::HW_INFO, "HW_INFO", b"", T)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("helper_not_ava1"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timed_out_call_leaves_the_session_usable() {
    let slow = Arc::new(AtomicBool::new(true));
    let s2 = slow.clone();
    let (t, p, c) = console(
        "timeout",
        Box::new(move |_, body| {
            if text_of(body) == "slow" && s2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(700));
            }
            text("fine")
        }),
    )
    .await;
    let e = call(
        &t,
        &c,
        m::HW_INFO,
        "HW_INFO",
        b"slow",
        Duration::from_millis(150),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
    let r = call(&t, &c, m::HW_INFO, "HW_INFO", b"quick", T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, b"fine");
    assert_eq!(p.attempts(), 1);
    let (g, r) = t.gate(&c).available();
    assert_eq!(
        (g, r),
        (GENERAL, 2),
        "the abandoned call gave its permit back"
    );
}

/// Under load: six slow calls hold every general slot and four more queue behind them, yet
/// `node.status` and `job.cancel` still go through, and the console never sees more than
/// six slow calls at once.
#[tokio::test(flavor = "multi_thread")]
async fn status_and_cancel_are_never_starved_by_slow_calls() {
    let release = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r2, i2, p2) = (release.clone(), inflight.clone(), peak.clone());
    let (t, _p, c) = console(
        "load",
        Box::new(move |method, _| match method {
            gen::METHOD_NODE_STATUS => ok(gen::NodeStatus::default().to_bytes().unwrap()),
            gen::METHOD_JOB_CANCEL => text(""), // carried as text until Task 5 types the job calls
            _ => {
                let now = i2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(now, Ordering::SeqCst);
                let start = Instant::now();
                while !r2.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(20) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                i2.fetch_sub(1, Ordering::SeqCst);
                text("slow done")
            }
        }),
    )
    .await;
    let slow: Vec<_> = (0..10)
        .map(|_| {
            let (t, c) = (t.clone(), c.clone());
            tokio::spawn(async move {
                call(&t, &c, m::HW_INFO, "HW_INFO", b"", Duration::from_secs(30)).await
            })
        })
        .collect();
    // Wait until six slow calls are inside the console.
    let start = Instant::now();
    while inflight.load(Ordering::SeqCst) < GENERAL {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "slow calls never arrived"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        inflight.load(Ordering::SeqCst),
        GENERAL,
        "the other four wait at the gate"
    );

    let st = call(
        &t,
        &c,
        m::NODE_STATUS,
        "STATUS",
        b"",
        Duration::from_secs(5),
    )
    .await;
    assert!(st.is_ok(), "node.status starved: {st:?}");
    let jc = call(
        &t,
        &c,
        JOB_CANCEL,
        "FS_OP_CANCEL",
        b"",
        Duration::from_secs(5),
    )
    .await;
    assert!(jc.is_ok(), "job.cancel starved: {jc:?}");

    release.store(true, Ordering::SeqCst);
    for h in slow {
        assert_eq!(h.await.unwrap().unwrap().unwrap(), b"slow done");
    }
    assert!(
        peak.load(Ordering::SeqCst) <= GENERAL,
        "peak {}",
        peak.load(Ordering::SeqCst)
    );
}

/// Ten callers find no session at the same moment: exactly one connects. A second connect by
/// the same identity would end the first's session (SPEC.md section 8) and the calls would fail.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_calls_open_one_session() {
    let (t, p, c) = console("race", Box::new(|_, _| text("ok"))).await;
    let calls: Vec<_> = (0..10)
        .map(|_| {
            let (t, c) = (t.clone(), c.clone());
            tokio::spawn(async move { call(&t, &c, m::HW_INFO, "HW_INFO", b"", T).await })
        })
        .collect();
    for h in calls {
        assert_eq!(h.await.unwrap().unwrap().unwrap(), b"ok");
    }
    assert_eq!(p.attempts(), 1);
}

/// A failure after an accepted chunk leaves `<path>.ps5upload.tmp` on the console; the writer
/// removes it best-effort with a `job.run` DELETE and still reports the write's own error.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_multi_chunk_write_removes_its_tmp_and_reports_the_write_error() {
    let n = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::<(u16, Vec<u8>)>::new()));
    let (n2, s2) = (n.clone(), seen.clone());
    let (t, _p, c) = console(
        "wtmp",
        Box::new(move |method, body| {
            s2.lock().unwrap().push((method, body.to_vec()));
            if method == gen::METHOD_JOB_RUN {
                // The cleanup job: one status poll later it is done.
                let r = gen::JobRun::decode(body).unwrap();
                return ok(job_status(r.job_id, 1).to_bytes().unwrap());
            }
            if method == gen::METHOD_JOB_STATUS {
                let r = gen::JobRef::decode(body).unwrap();
                return ok(job_status(r.job_id, 1).to_bytes().unwrap());
            }
            if n2.fetch_add(1, Ordering::SeqCst) == 1 {
                err(gen::ERR_NO_SPACE, "no_space")
            } else {
                ok(vec![])
            }
        }),
    )
    .await;
    let size = 3 * gen::FSW_CHUNK_MAX as usize;
    let e = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/data/p", size, "overwrite"),
        T,
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "payload rejected FS_WRITE_BYTES: no_space");
    let seen = seen.lock().unwrap();
    let methods: Vec<u16> = seen.iter().map(|s| s.0).collect();
    assert_eq!(
        methods[..3],
        [
            gen::METHOD_FS_WRITE,
            gen::METHOD_FS_WRITE,
            gen::METHOD_JOB_RUN
        ],
        "stopped at the failed chunk, then removed the tmp"
    );
    let run = gen::JobRun::decode(&seen[2].1).unwrap();
    assert_eq!(run.op, gen::JOB_OP_DELETE);
    let args: serde_json::Value = serde_json::from_slice(&run.args).unwrap();
    assert_eq!(args["path"], "/data/p.ps5upload.tmp");
}

/// A refusal of the FIRST chunk names a tmp file this write did not create: nothing is removed.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_first_chunk_removes_nothing() {
    let methods = Arc::new(Mutex::new(Vec::<u16>::new()));
    let m2 = methods.clone();
    let (t, _p, c) = console(
        "wtmp0",
        Box::new(move |method, _| {
            m2.lock().unwrap().push(method);
            err(gen::ERR_EXISTS, "exists")
        }),
    )
    .await;
    let size = 3 * gen::FSW_CHUNK_MAX as usize;
    let e = call(
        &t,
        &c,
        m::FS_WRITE,
        "FS_WRITE_BYTES",
        &write_body("/data/p", size, "create"),
        T,
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("exists"), "{e}");
    assert_eq!(*methods.lock().unwrap(), vec![gen::METHOD_FS_WRITE]);
}

/// A finished job's Status, as the console answers `job.run` / `job.status`.
fn job_status(job_id: [u8; 16], state: u8) -> gen::Status {
    gen::Status {
        job_id,
        files_done: 0,
        files_total: 0,
        bytes_received: 0,
        bytes_durable: 0,
        bytes_total: 0,
        bottleneck: 0,
        workers: 0,
        lanes: 0,
        sequential: 0,
        current: None,
        state: Some(state),
        result: None,
        code: None,
        unswept: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_ports_of_one_console_share_one_gate() {
    let (t, _p, _c) = console("gate", Box::new(|_, _| text("x"))).await;
    assert!(Arc::ptr_eq(&t.gate("10.1.1.1"), &t.gate("10.1.1.1:9120")));
    assert!(!Arc::ptr_eq(&t.gate("10.1.1.1"), &t.gate("10.1.1.2")));
}

/// `call` is blocking, but a caller on an async worker must not panic the runtime.
#[tokio::test(flavor = "multi_thread")]
async fn call_from_a_multi_thread_worker_does_not_panic() {
    let (t, _p, c) = console("worker", Box::new(|_, _| text("fine"))).await;
    let r = t.call(&c, m::HW_INFO, "HW_INFO", b"", T).unwrap();
    assert_eq!(r.unwrap(), b"fine");
}

#[test]
fn call_from_a_current_thread_runtime_does_not_panic() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (t, c) = rt.block_on(async {
        let (t, _p, c) = console("curthread", Box::new(|_, _| text("fine"))).await;
        (t, c)
    });
    let cur = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let r = cur.block_on(async { t.call(&c, m::HW_INFO, "HW_INFO", b"", T) });
    assert_eq!(r.unwrap().unwrap(), b"fine");
    drop(rt);
}

// ---- P3 Task 9: the log tails ----

#[tokio::test(flavor = "multi_thread")]
async fn a_clamped_log_tail_is_led_by_a_note_and_a_whole_one_is_not() {
    let (t, _p, c) = console(
        "tails",
        Box::new(|method, body| {
            let n: usize = text_of(body).parse().unwrap_or(0);
            let more = match method {
                gen::METHOD_LOG_SYSLOG => Some(1),
                gen::METHOD_LOG_KLOG => Some(0),
                _ => None,
            };
            ok(MgmtText {
                body: vec![b'x'; n],
                more,
            }
            .to_bytes()
            .unwrap())
        }),
    )
    .await;
    // the console clamped it (`more` = 1): the text says so, then the newest part follows
    let r = call(&t, &c, m::LOG_SYSLOG, "SYSLOG_TAIL", b"5", T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        r,
        [ps5upload_ava1::mgmt::TAIL_CLIPPED.as_bytes(), b"xxxxx"].concat()
    );
    // not clamped (`more` = 0): the bytes untouched
    let r = call(&t, &c, m::LOG_KLOG, "KLOG_READ", b"5", T)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, b"xxxxx");
}

/// A management call carries its caller's bound all the way down: the session's own default
/// (`RPC_TIMEOUT`, for callers that name none) must not cut a call the caller allowed longer.
/// It did: `ava1-calibrate` on a console's internal drive (about 90 s) failed at 60 s.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_longer_than_the_session_default_gets_its_callers_bound() {
    let held = ava1::session::RPC_TIMEOUT + Duration::from_secs(3);
    let (t, _p, c) = console(
        "long",
        Box::new(move |_, _| {
            std::thread::sleep(held);
            text("done")
        }),
    )
    .await;
    let r = call(
        &t,
        &c,
        m::HW_INFO,
        "HW_INFO",
        b"",
        held + Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(r.unwrap(), b"done");
}
