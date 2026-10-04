#![cfg(unix)]
//! P3 Task 2: the management dispatcher (payload/src/mgmt_rpc.c) driven over AVA1 against
//! stub handlers, plus static audits of the real table (payload/src/mgmt_table.def).
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen::{self, FsMkdir, MgmtText};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session, Timing};
use ava1::wire::Message;
use ava1_ctest::*;

const SECRET: [u8; 32] = [0x42; 32];
const OK: u16 = gen::STATUS_OK;

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(2000),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-mgmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A started C server with the stub table installed and a paired client.
/// The stub table, its counters and the C server are process-wide: tests of this file run one at a
/// time (the lock is held for the rig's life; the server drops first).
struct Rig {
    _srv: CServer,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn rig_lock() -> std::sync::MutexGuard<'static, ()> {
    // one serialisation scheme for every shim-global test: the shared guard (src/lib.rs)
    CServer::lock_for_shim_tests()
}

async fn rig(tag: &str) -> (Rig, Session) {
    rig_with(tag, true).await
}

/// `install = false`: the server starts with no management table (no CAP_MGMT).
// The guard serialises whole tests, each on its own runtime; no other task of the test waits on it.
#[allow(clippy::await_holding_lock)]
async fn rig_with(tag: &str, install: bool) -> (Rig, Session) {
    let lock = rig_lock();
    if install {
        assert_eq!(mgmt::install(), 0);
    } else {
        mgmt::uninstall();
    }
    let d = dir(tag);
    let me = Arc::new(Identity::generate().unwrap());
    PeerStore::load(&d.join("peers"))
        .unwrap()
        .add(me.public(), "rust client")
        .unwrap();
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 2000, 500);
    let s = connect(
        &srv.addr(),
        me,
        Arc::new(Mutex::new(mine)),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    (
        Rig {
            _srv: srv,
            _lock: lock,
        },
        s,
    )
}

fn text(s: &str) -> Vec<u8> {
    MgmtText {
        body: s.as_bytes().to_vec(),
        more: None,
    }
    .to_bytes()
    .unwrap()
}

fn untext(b: &[u8]) -> MgmtText {
    MgmtText::decode(b).expect("a MgmtText reply")
}

fn mkdir(path: &str) -> Vec<u8> {
    FsMkdir {
        path: path.into(),
        mode: 0o755,
        parents: 1,
    }
    .to_bytes()
    .unwrap()
}

const VOLUMES: u16 = gen::METHOD_FS_VOLUMES;
const MKDIR: u16 = gen::METHOD_FS_MKDIR;
const LAUNCH: u16 = gen::METHOD_APP_LAUNCH;
const APP_LIST: u16 = gen::METHOD_APP_LIST;
const BIG: u16 = gen::METHOD_PROC_PROCESS_LIST;
const ENV: u16 = gen::METHOD_FS_MOUNT;
const TWO: u16 = gen::METHOD_FS_UNMOUNT;
const NODE_STATUS: u16 = gen::METHOD_NODE_STATUS;
const SILENT: u16 = gen::METHOD_FS_MOUNT_PKG;

// ---- the table ----

fn payload() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../payload")
}

/// Runs one check of payload/tools/mgmt_audit.py; its output is the failure message.
fn audit(check: &str) {
    let out = Command::new("python3")
        .arg(payload().join("tools/mgmt_audit.py"))
        .arg(check)
        .output()
        .expect("python3 runs payload/tools/mgmt_audit.py");
    assert!(
        out.status.success(),
        "mgmt_audit.py {check}:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The real table's lines: (method name, flags).
fn real_table() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(payload().join("src/mgmt_table.def")).unwrap();
    src.lines()
        .filter(|l| l.starts_with("MGMT_H") || l.starts_with("MGMT_N"))
        .map(|l| {
            let inner = &l[l.find('(').unwrap() + 1..l.rfind(')').unwrap()];
            let f: Vec<&str> = inner.split(',').map(str::trim).collect();
            // MGMT_H0/H1/HS: method, frame, ack, flags, handler, runner. MGMT_N: ..., run.
            assert_eq!(f.len(), if l.starts_with("MGMT_N") { 5 } else { 6 }, "{l}");
            (f[0].to_string(), f[3].to_string())
        })
        .collect()
}

#[test]
fn c_mgmt_table_has_no_duplicate_methods() {
    // The installer refuses a repeated method...
    assert_eq!(mgmt::install_duplicate(), -1);
    // ...and the real table has none, names real constants and real handlers.
    let mut names: Vec<String> = real_table().into_iter().map(|t| t.0).collect();
    let n = names.len();
    assert!(n >= 5, "the first slice is in the table");
    names.sort();
    names.dedup();
    assert_eq!(names.len(), n, "duplicate method in mgmt_table.def");
    audit("table");
}

#[test]
fn c_mgmt_table_flags_sony_methods() {
    // Every handler that can reach register/profile/registry/Remote Play/notification code or a
    // Sony API is flagged MGMT_SONY (the audit derives it from the call graph).
    audit("sony");
    let t = real_table();
    let flag = |m: &str| t.iter().find(|e| e.0 == m).unwrap().1.clone();
    assert_eq!(flag("AVA1_METHOD_APP_LAUNCH"), "MGMT_SONY");
    assert_eq!(flag("AVA1_METHOD_FS_MKDIR"), "0");
}

#[test]
fn c_mgmt_handlers_never_read_the_socket_and_keep_small_stacks() {
    // The capture path calls handlers with fd = -1: none may read client_fd after the header.
    audit("recv");
    // And no table handler reaches a stack array of 16 KiB or more (SPEC.md section 7.3).
    audit("stack");
}

// ---- the dispatcher ----

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_unknown_method_is_err_unknown_method() {
    let (_srv, s) = rig("unknown").await;
    // 97 and 99 are unassigned in the schema (backup snapshot/restore run as job.run ops)
    for m in [97u16, 99, 0x7fff] {
        let r = s.rpc(m, &[]).await.unwrap();
        assert_eq!(r.status, gen::ERR_UNKNOWN_METHOD, "method {m}");
        assert_eq!(r.body, b"unknown method");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_text_method_answers_a_mgmt_text() {
    let (_srv, s) = rig("text").await;
    let r = s.rpc(VOLUMES, &[]).await.unwrap();
    assert_eq!(r.status, OK);
    let t = untext(&r.body);
    assert_eq!(t.body, br#"{"volumes":[{"path":"/data"}]}"#);
    assert_eq!(t.more, None);
    // A request that is not a MgmtText is the peer's error.
    let r = s.rpc(VOLUMES, &[1, 2, 3]).await.unwrap();
    assert_eq!(r.status, gen::ERR_PROTOCOL);
    assert_eq!(r.body, b"bad MgmtText request");
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_error_frame_becomes_status_and_cause() {
    let (_srv, s) = rig("errors").await;
    // typed fs method: ok, and the path reaches the legacy handler JSON-escaped
    let r = s.rpc(MKDIR, &mkdir("/data/a")).await.unwrap();
    assert_eq!((r.status, r.body.len()), (OK, 0));
    assert_eq!(mgmt::last_path(), "/data/a");
    let r = s.rpc(MKDIR, &mkdir("/data/q\"uote")).await.unwrap();
    assert_eq!(r.status, OK);
    assert_eq!(mgmt::last_path(), "/data/q\\\"uote");
    // legacy ERROR frames: the token is the cause, the status is the closest ERR_*
    let r = s.rpc(MKDIR, &mkdir("/denied")).await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::ERR_PATH, &b"fs_mkdir_path_not_allowed"[..])
    );
    let r = s.rpc(MKDIR, &mkdir("/fail")).await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::ERR_IO, &b"fs_mkdir_failed"[..])
    );
    let r = s.rpc(MKDIR, &[0xff]).await.unwrap();
    assert_eq!(r.status, gen::ERR_PROTOCOL);
    // a successful frame whose body is {"ok":false,...} is an error status, never OK
    let r = s
        .rpc(LAUNCH, &text(r#"{"title_id":"NOPE00001"}"#))
        .await
        .unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (
            gen::ERR_INTERNAL,
            &br#"{"ok":false,"err":"launch_failed"}"#[..]
        )
    );
    // a missing argument is the peer's
    let r = s.rpc(LAUNCH, &text("{}")).await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::ERR_PROTOCOL, &b"launch_title_id_missing"[..])
    );
    // the first error frame wins over a later success frame
    let r = s.rpc(TWO, &text("{}")).await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::ERR_INTERNAL, &b"fs_unmount_failed"[..])
    );
    // a handler that sends nothing is an error, not an empty OK
    let r = s.rpc(SILENT, &text("{}")).await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::ERR_INTERNAL, &b"handler sent no reply"[..])
    );
    // success still works
    let r = s
        .rpc(LAUNCH, &text(r#"{"title_id":"PPSA00001"}"#))
        .await
        .unwrap();
    assert_eq!((r.status, untext(&r.body).body.len()), (OK, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_reply_over_cap_is_an_error_not_a_truncation() {
    let (_srv, s) = rig("cap").await;
    let max = gen::RPC_TEXT_MAX as usize;
    let ask = |n: usize| text(&format!("{{\"n\":{n}}}"));
    // exactly RPC_TEXT_MAX bytes of text fit a reply
    let r = s.rpc(BIG, &ask(max)).await.unwrap();
    assert_eq!(r.status, OK);
    let t = untext(&r.body);
    assert_eq!(t.body.len(), max);
    assert!(t.body.iter().all(|b| *b == b'x'));
    // one byte more, and far more, are ERR_INTERNAL "reply truncated", never a clipped OK
    for n in [max + 1, max + 16, 400_000] {
        let r = s.rpc(BIG, &ask(n)).await.unwrap();
        assert_eq!(r.status, gen::ERR_INTERNAL, "n = {n}");
        assert_eq!(r.body, b"reply truncated");
    }
    // the session carries on
    assert_eq!(s.rpc(VOLUMES, &[]).await.unwrap().status, OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_workers_use_512k_stacks_and_elevate() {
    let (_srv, s) = rig("env").await;
    let (e0, l0, _, _) = mgmt::stats();
    let r = s.rpc(ENV, &text("{}")).await.unwrap();
    assert_eq!(r.status, OK);
    let body = String::from_utf8(untext(&r.body).body).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let stack = v["stack"].as_u64().unwrap() as usize;
    // 512 KiB (macOS pads the reported allocation by up to ~64 KiB)
    assert!(
        (512 * 1024..=512 * 1024 + 64 * 1024).contains(&stack),
        "stack {stack}"
    );
    // the environment hook ran on the handler's own thread, with the legacy frame number
    assert_eq!(
        v["marker"].as_u64().unwrap(),
        52,
        "enter() set the marker the handler saw"
    );
    let (e1, l1, last, _) = mgmt::stats();
    assert_eq!(
        (e1 - e0, l1 - l0, last),
        (1, 1, 52),
        "enter and leave ran once, for FsMount"
    );
    // a data-plane method number keeps the 256 KiB worker
    let r = s.rpc(19, &[]).await.unwrap();
    assert_eq!(r.status, OK);
    let small: usize = String::from_utf8(r.body).unwrap().parse().unwrap();
    // ASan gives every thread at least 512 KiB (it pads stacks for its redzones), so under
    // AVA1_CTEST_SANITIZE the data-plane worker reads as 512 KiB too: the size is not ours to measure there.
    let cap = if cfg!(ava1_ctest_sanitize) {
        512 * 1024 + 64 * 1024
    } else {
        256 * 1024 + 64 * 1024
    };
    assert!(small <= cap, "method 19 stack {small}");
    // errors leave the environment too
    let _ = s.rpc(MKDIR, &mkdir("/denied")).await.unwrap();
    let (e2, l2, _, _) = mgmt::stats();
    assert_eq!(e2 - e1, l2 - l1);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_sony_method_calls_are_serialised_by_the_handler_not_the_dispatcher() {
    // Four concurrent calls reach the handler; the stub's stand-in for sony_api_lock never
    // sees two inside at once. (The dispatcher adds no lock and no Sony call of its own.)
    let (_srv, s) = rig("sony").await;
    let s = Arc::new(s);
    let mut js = Vec::new();
    for _ in 0..4 {
        let s = s.clone();
        js.push(tokio::spawn(async move {
            s.rpc(LAUNCH, &text(r#"{"title_id":"PPSA00001"}"#))
                .await
                .unwrap()
                .status
        }));
    }
    for j in js {
        assert_eq!(j.await.unwrap(), OK);
    }
    assert_eq!(mgmt::stats().3, 1);
}

fn app_ids(t: &MgmtText) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_slice(&t.body).expect("a page is valid JSON");
    v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_app_list_pages_cover_every_entry() {
    let (_srv, s) = rig("pages").await;
    // 6,000 entries is more than one 256 KiB reply holds (the handler's own buffer is bigger).
    mgmt::set_apps(6000);
    let all: Vec<String> = (0..6000).map(|i| format!("PPSA{i:05}")).collect();

    // no limit: the node fills each reply and says `more`
    let mut got = Vec::new();
    let mut calls = 0;
    loop {
        let req = text(&format!("{{\"offset\":{}}}", got.len()));
        let r = s.rpc(APP_LIST, &req).await.unwrap();
        assert_eq!(r.status, OK, "{}", String::from_utf8_lossy(&r.body));
        let t = untext(&r.body);
        assert!(t.body.len() <= gen::RPC_TEXT_MAX as usize);
        let ids = app_ids(&t);
        assert!(!ids.is_empty());
        got.extend(ids);
        calls += 1;
        match t.more {
            Some(1) => continue,
            Some(0) | None => break,
            m => panic!("more = {m:?}"),
        }
    }
    assert!(
        calls >= 2,
        "6,000 entries must not fit one reply (took {calls})"
    );
    assert_eq!(got, all, "every entry exactly once, in order");

    // an explicit limit
    let r = s
        .rpc(APP_LIST, &text(r#"{"offset":10,"limit":3}"#))
        .await
        .unwrap();
    let t = untext(&r.body);
    assert_eq!(app_ids(&t), ["PPSA00010", "PPSA00011", "PPSA00012"]);
    assert_eq!(t.more, Some(1));
    // the last page: no more
    let r = s
        .rpc(APP_LIST, &text(r#"{"offset":5998,"limit":50}"#))
        .await
        .unwrap();
    let t = untext(&r.body);
    assert_eq!(app_ids(&t), ["PPSA05998", "PPSA05999"]);
    assert_eq!(t.more, Some(0));
    // past the end: an empty array, not an error
    let r = s.rpc(APP_LIST, &text(r#"{"offset":9999}"#)).await.unwrap();
    assert_eq!(r.status, OK);
    let t = untext(&r.body);
    assert!(app_ids(&t).is_empty());
    assert_eq!(t.more, Some(0));

    // a small list is one reply, with the legacy shape intact
    mgmt::set_apps(2);
    let r = s.rpc(APP_LIST, &[]).await.unwrap();
    let t = untext(&r.body);
    assert_eq!(app_ids(&t), ["PPSA00000", "PPSA00001"]);
    assert_eq!(t.more, Some(0));
    mgmt::set_apps(0);
    let r = s.rpc(APP_LIST, &[]).await.unwrap();
    assert_eq!(untext(&r.body).body, br#"{"apps":[]}"#);
}

// ---- node.status (typed) ----

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_node_status_is_typed() {
    let (_srv, s) = rig("status").await;
    let r = s.rpc(NODE_STATUS, &[]).await.unwrap();
    assert_eq!(r.status, OK, "{}", String::from_utf8_lossy(&r.body));
    let n = gen::NodeStatus::decode(&r.body).expect("a NodeStatus reply");
    assert_eq!(n.version, "9.9.9");
    assert_eq!(n.ps5_kernel, "FreeBSD \"11\" test"); // the legacy JSON escape is undone
    assert_eq!(n.instance_id, 18_446_744_073_709_551_000);
    assert_eq!(n.started_at_unix, 1_700_000_000);
    assert_eq!(n.command_count, 5);
    assert_eq!(n.startup_reason, 2);
    assert_eq!(n.ucred_elevated, 1);
    assert_eq!(n.max_transfer_streams, 4);
    assert_eq!(n.fan_threshold, 70);
    assert_eq!(n.fan_reapply_sec, 30);
    assert_eq!(n.prior_instance.as_deref(), Some("killed_externally"));
    // a request body is ignored; the reply is the same shape
    assert_eq!(s.rpc(NODE_STATUS, &[1, 2]).await.unwrap().status, OK);
}

// ---- CAP_MGMT ----

#[test]
fn cap_mgmt_is_bit_one() {
    assert_eq!(gen::CAP_MGMT, 2);
    assert_eq!(gen::CAP_MGMT & gen::CAP_DATA_PLANE, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_advertises_cap_mgmt_only_with_the_dispatcher() {
    let (srv, s) = rig_with("capoff", false).await;
    assert!(!s.has_mgmt(), "no table installed: no CAP_MGMT");
    assert_eq!(s.peer_caps() & gen::CAP_MGMT, 0);
    drop((s, srv));
    let (_srv, s) = rig_with("capon", true).await;
    assert!(s.has_mgmt());
    assert_eq!(s.peer_caps() & gen::CAP_MGMT, gen::CAP_MGMT);
    // the data-plane bit is independent (this server has no data hooks)
    assert_eq!(s.peer_caps() & gen::CAP_DATA_PLANE, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_server_advertises_cap_mgmt_with_with_mgmt() {
    use ava1::server::{self, ServerCtx};
    use ava1::session::RpcReply;
    for with in [false, true] {
        let me = Arc::new(Identity::generate().unwrap());
        let mut peers = PeerStore::in_memory();
        peers.add(me.public(), "client").unwrap();
        let rpc: server::RpcHandler = Box::new(|_, _| RpcReply {
            status: OK,
            body: Vec::new(),
        });
        let mut ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc);
        if with {
            ctx = ctx.with_mgmt();
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let task = tokio::spawn(server::serve(l, Arc::new(ctx)));
        // connecting adds the host's key to our store, so one store serves the whole test
        let mine = Arc::new(Mutex::new(PeerStore::in_memory()));
        let s = connect(&addr, me, mine, "c", fast()).await.unwrap();
        assert_eq!(s.has_mgmt(), with);
        task.abort();
    }
}

// ---- minors ----

#[tokio::test(flavor = "multi_thread")]
async fn c_mgmt_bad_or_overflowing_page_arguments_are_protocol_errors() {
    let (_srv, s) = rig("badpage").await;
    mgmt::set_apps(10);
    for body in [
        r#"{"offset":"x"}"#,
        r#"{"offset":-1}"#,
        r#"{"offset":99999999999999999999999}"#,
        r#"{"offset":1.5}"#,
        r#"{"limit":-2}"#,
        r#"{"limit":"all"}"#,
        r#"{"offset":}"#,
    ] {
        let r = s.rpc(APP_LIST, &text(body)).await.unwrap();
        assert_eq!(r.status, gen::ERR_PROTOCOL, "{body}");
        assert!(
            r.body == b"bad offset" || r.body == b"bad limit",
            "{body}: {}",
            String::from_utf8_lossy(&r.body)
        );
    }
    // absent is fine, and so is a spaced valid number
    assert_eq!(s.rpc(APP_LIST, &text("{}")).await.unwrap().status, OK);
    assert_eq!(
        s.rpc(APP_LIST, &text(r#"{"offset" : 3, "limit" : 2}"#))
            .await
            .unwrap()
            .status,
        OK
    );
    mgmt::set_apps(0);
}

#[test]
fn legacy_tokens_map_to_the_closest_status() {
    let cases = [
        ("fs_move_cross_mount", gen::ERR_CROSS_DEVICE),
        // policy refusals of a path stay ERR_PATH...
        ("fs_list_dir_path_denied", gen::ERR_PATH),
        ("cleanup_path_denied", gen::ERR_PATH),
        ("fs_mkdir_path_not_allowed", gen::ERR_PATH),
        // ...an OS permission refusal is not a path problem (the schema has no permission code: ERR_IO)
        ("permission_denied", gen::ERR_IO),
        ("access_denied", gen::ERR_IO),
        ("eacces", gen::ERR_IO),
        ("eperm", gen::ERR_IO),
        ("fs_write_failed_errno_28", gen::ERR_IO),
        ("disk_full", gen::ERR_NO_SPACE),
        // Ambiguous pairs (review L5): the first matching rule wins, so a substring that
        // two rules share resolves by order. "already_running" is BUSY, bare "already"
        // is EXISTS; "invalid_path" is PATH (the path rule precedes "invalid" -> PROTOCOL).
        ("already_running", gen::ERR_BUSY),
        ("already", gen::ERR_EXISTS),
        ("already_exists", gen::ERR_EXISTS),
        ("invalid_path", gen::ERR_PATH),
        ("invalid_title_id", gen::ERR_PROTOCOL),
        ("launch_title_id_missing", gen::ERR_PROTOCOL),
        ("something_else", gen::ERR_INTERNAL),
    ];
    for (tok, want) in cases {
        assert_eq!(mgmt::status_for_token(tok), want as i32, "{tok}");
    }
}

#[test]
fn the_environment_hook_counts_no_command() {
    // runtime.c's handlers bump command_count themselves (as they did under FTX2); the hook that
    // runs around them must not add a second count.
    let inc = std::fs::read_to_string(payload().join("src/mgmt_install.inc")).unwrap();
    assert!(
        !inc.contains("command_count"),
        "mgmt_install.inc must not touch command_count"
    );
}

// ---- the audit script is a tripwire, and these are the shapes it must see ----

#[test]
fn mgmt_audit_sees_struct_2d_pointer_and_function_pointer_cases() {
    audit("selftest");
}
