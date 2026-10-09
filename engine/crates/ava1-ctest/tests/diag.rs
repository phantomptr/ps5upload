#![cfg(unix)]
//! P3 Task 9: bug-report log reads, the crash black box and the diagnostics probes over
//! AVA1, against the C server on loopback.
//!
//! What is real C here: the dispatcher and its two new runners (`mgmt_call_tail`: a clamped
//! tail, `mgmt_call_probe`: a negative answer is data), the net.reach probe
//! (`payload/src/net_probe.c`, a real TCP connect), and the AVA1 event log
//! (`payload/ava1/ava1_events.c`). The kernel-side handlers (`/dev/klog`, `kern.msgbuf`,
//! `sceNetGetIfList`) exist only on a console, so the table uses stubs that answer with the
//! same bodies and limits as `runtime.c`'s handlers.
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::gen::{self, MgmtText};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session, Timing};
use ava1::wire::Message;
use ava1_ctest::*;
use ps5upload_ava1::mgmt::{AvaTransport, TAIL_CLIPPED};
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt as cmgmt;
use ps5upload_core::mgmt::{m, Method, MgmtTransport};
use ps5upload_core::{diagnostics, hw};

const SECRET: [u8; 32] = [0x42; 32];
const OK: u16 = gen::STATUS_OK;
/// The most text one reply carries: the reply cap (256 KiB, SPEC.md section 7.4) minus 16
/// (`RPC_TEXT_MAX`), which also covers the MgmtText framing.
const TEXT_CAP: usize = 256 * 1024 - 16;

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(2000),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

fn dir(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-diag-{tag}-{}", std::process::id()))
}

/// The stub's log: lines of 49 bytes, numbered, `n` bytes in all (test_shim.c `numbered_log`).
fn numbered_log(n: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < n {
        let line = format!("line {i:08} ..............................\n");
        let take = line.len().min(n - s.len());
        s.push_str(&line[..take]);
        i += 1;
    }
    s
}

struct Rig {
    srv: CServer,
    me: Arc<Identity>,
    mine: Arc<Mutex<PeerStore>>,
    ava: PathBuf,
    // The scratch dir: declared after the server so the server stops before it goes away.
    _d: TempDir,
    // Declared last so it drops last: the server stops before the next test may start one.
    _one_at_a_time: std::sync::MutexGuard<'static, ()>,
}

/// The installed management table, its counters and the C server's dispatcher are process-wide:
/// tests that start a server run one at a time. Lock order: RIG, then EVENTS.
static RIG: Mutex<()> = Mutex::new(());

fn start(tag: &str) -> Rig {
    let one_at_a_time = RIG.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(mgmt::install_diag(), 0);
    let d = dir(tag);
    let ava = d.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Arc::new(Identity::load_or_create(&ava.join("identity")).unwrap());
    PeerStore::load(&d.join("peers"))
        .unwrap()
        .add(me.public(), "rust client")
        .unwrap();
    PeerStore::load(&ava.join("peers"))
        .unwrap()
        .add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 2000, 500);
    Rig {
        srv,
        me,
        mine: Arc::new(Mutex::new(mine)),
        ava,
        _d: d,
        _one_at_a_time: one_at_a_time,
    }
}

async fn raw(r: &Rig) -> Session {
    connect(
        &r.srv.addr(),
        r.me.clone(),
        r.mine.clone(),
        "laptop",
        fast(),
    )
    .await
    .unwrap()
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

/// The engine's transport over its own pool, aimed at the C server.
fn transport(r: &Rig) -> Arc<AvaTransport> {
    let pool: &'static Pool = keep(Box::leak(Box::new(
        Pool::new(r.ava.clone()).with_addr(r.srv.addr()),
    )));
    Arc::new(AvaTransport::with_pool(pool))
}

// ---- the tail window (pure) ----

#[test]
fn c_tail_window_keeps_a_short_text_whole_and_cuts_a_long_one_at_a_line() {
    // fits: no clip
    assert_eq!(mgmt::tail_window(b"abc\ndef\n", 100), (false, 0));
    assert_eq!(mgmt::tail_window(b"abc\ndef\n", 8), (false, 0));
    // one byte over: clipped, and the window starts right after a newline (not mid-line)
    let t = b"aaaa\nbbbb\ncccc\n"; // 15 bytes
    let (clipped, start) = mgmt::tail_window(t, 12);
    assert!(clipped);
    assert_eq!(&t[start..], b"bbbb\ncccc\n");
    // no newline anywhere near the cut: never start inside a UTF-8 sequence
    let u = "ab".to_string() + &"\u{20ac}".repeat(50); // 3-byte characters, no newline
    let (clipped, start) = mgmt::tail_window(u.as_bytes(), 100);
    assert!(clipped);
    assert!(std::str::from_utf8(&u.as_bytes()[start..]).is_ok());
    assert!(u.len() - start <= 100);
}

// ---- the methods, over the wire ----

#[tokio::test(flavor = "multi_thread")]
async fn c_klog_returns_what_was_asked_up_to_its_64_kib_ceiling() {
    let r = start("klog");
    let s = raw(&r).await;
    // an exact request is answered whole, without `more`
    let a = s
        .rpc(gen::METHOD_LOG_KLOG, &text(r#"{"max_bytes":1000}"#))
        .await
        .unwrap();
    assert_eq!(a.status, OK);
    let t = untext(&a.body);
    assert_eq!(t.body, numbered_log(1000).as_bytes());
    assert_eq!(t.more.unwrap_or(0), 0);
    // no body: the handler's own default of 16 KiB
    let d = s.rpc(gen::METHOD_LOG_KLOG, &[]).await.unwrap();
    assert_eq!(d.status, OK);
    assert_eq!(untext(&d.body).body.len(), 16 * 1024);
    // 64 KiB is the handler's ceiling and fits one reply: whole, not clamped
    let big = s
        .rpc(gen::METHOD_LOG_KLOG, &text(r#"{"max_bytes":1000000}"#))
        .await
        .unwrap();
    assert_eq!(big.status, OK);
    let t = untext(&big.body);
    assert_eq!(t.body, numbered_log(64 * 1024).as_bytes());
    assert_eq!(t.more.unwrap_or(0), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_syslog_is_a_clamped_tail_never_a_clipped_ok() {
    let r = start("syslog");
    let s = raw(&r).await;
    for len in [300_000u32, 1 << 20] {
        mgmt::set_syslog(len, 0);
        let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
        assert_eq!(a.status, OK, "{len} bytes");
        let t = untext(&a.body);
        let full = numbered_log(len as usize);
        assert_eq!(t.more, Some(1), "{len}: older text was left out");
        assert!(t.body.len() <= TEXT_CAP && t.body.len() > TEXT_CAP - 4096);
        assert!(
            full.as_bytes().ends_with(&t.body),
            "{len}: the newest bytes"
        );
        assert!(t.body.starts_with(b"line "), "{len}: from a line start");
    }
    // a typical kernel buffer (a few hundred KiB at most) that fits is whole and unmarked
    mgmt::set_syslog(200_000, 0);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    let t = untext(&a.body);
    assert_eq!((t.body.len(), t.more.unwrap_or(0)), (200_000, 0));
    // a short buffer comes back whole and unmarked
    mgmt::set_syslog(500, 0);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    let t = untext(&a.body);
    assert_eq!(
        (t.body, t.more.unwrap_or(0)),
        (numbered_log(500).into_bytes(), 0)
    );
    // exactly the reply's room: still whole
    mgmt::set_syslog(TEXT_CAP as u32, 0);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    let t = untext(&a.body);
    assert_eq!((t.body.len(), t.more.unwrap_or(0)), (TEXT_CAP, 0));
    // one byte more: clipped, marked
    mgmt::set_syslog(TEXT_CAP as u32 + 1, 0);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    assert_eq!(untext(&a.body).more, Some(1));
    // an empty buffer is a successful read of nothing
    mgmt::set_syslog(0, 2);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    assert_eq!(a.status, OK);
    assert!(untext(&a.body).body.is_empty());
    // the sysctl failing keeps its errno token as the cause
    mgmt::set_syslog(0, 1);
    let a = s.rpc(gen::METHOD_LOG_SYSLOG, &[]).await.unwrap();
    assert_eq!(a.status, gen::ERR_IO);
    assert_eq!(a.body, b"syslog_tail_sysctl_errno_12");
    mgmt::set_syslog(200_000, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_net_interfaces_speedtest_and_modules_answer_text() {
    let r = start("text");
    let s = raw(&r).await;
    let a = s.rpc(gen::METHOD_NET_INTERFACES, &[]).await.unwrap();
    assert_eq!(a.status, OK);
    let j: serde_json::Value = serde_json::from_slice(&untext(&a.body).body).unwrap();
    assert_eq!(j["interfaces"][0]["ipv4"], "192.168.1.50");
    let a = s.rpc(gen::METHOD_NET_SPEEDTEST, &[]).await.unwrap();
    assert_eq!(
        (a.status, untext(&a.body).body),
        (OK, b"{\"ok\":true}".to_vec())
    );
    let a = s
        .rpc(gen::METHOD_PROC_MODULES, &text(r#"{"pid":0}"#))
        .await
        .unwrap();
    assert_eq!(a.status, OK);
    let j: serde_json::Value = serde_json::from_slice(&untext(&a.body).body).unwrap();
    assert_eq!(j["modules"][0]["name"], "libkernel.sprx");
    // a request that is not a MgmtText is the peer's error
    let a = s.rpc(gen::METHOD_NET_INTERFACES, &[1, 2, 3]).await.unwrap();
    assert_eq!(a.status, gen::ERR_PROTOCOL);
}

fn reach_body(host: &str, port: u16, timeout_ms: u32) -> Vec<u8> {
    text(&format!(
        r#"{{"host":"{host}","port":"{port}","timeout_ms":"{timeout_ms}"}}"#
    ))
}

fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port() // dropped: nothing listens
}

#[tokio::test(flavor = "multi_thread")]
async fn c_net_reach_runs_the_real_probe_and_a_refusal_is_an_answer_not_an_error() {
    let r = start("reach");
    let s = raw(&r).await;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let open = l.local_addr().unwrap().port();
    let a = s
        .rpc(gen::METHOD_NET_REACH, &reach_body("127.0.0.1", open, 2000))
        .await
        .unwrap();
    assert_eq!(a.status, OK);
    let j: serde_json::Value = serde_json::from_slice(&untext(&a.body).body).unwrap();
    assert_eq!(j["ok"], true);
    assert!(j["ms"].as_u64().unwrap() < 2000);
    // nothing listening: the measurement is {"ok":false,"errno":..} and it arrives as an OK reply
    let a = s
        .rpc(
            gen::METHOD_NET_REACH,
            &reach_body("127.0.0.1", closed_port(), 2000),
        )
        .await
        .unwrap();
    assert_eq!(a.status, OK, "a negative probe is data");
    let j: serde_json::Value = serde_json::from_slice(&untext(&a.body).body).unwrap();
    assert_eq!(j["ok"], false);
    assert_eq!(j["timed_out"], false);
    assert!(j["errno"].as_i64().unwrap() != 0, "{j}");
    assert!(j["ms"].is_number());
    // a request that is wrong is an error status with its token
    let a = s
        .rpc(gen::METHOD_NET_REACH, &text(r#"{"port":"80"}"#))
        .await
        .unwrap();
    assert_eq!(
        (a.status, a.body.as_slice()),
        (gen::ERR_PROTOCOL, &b"bad_request"[..])
    );
    let a = s
        .rpc(gen::METHOD_NET_REACH, &reach_body("999.1.1.1", 80, 100))
        .await
        .unwrap();
    assert_eq!(
        (a.status, a.body.as_slice()),
        (gen::ERR_PROTOCOL, &b"bad_address"[..])
    );
    let a = s
        .rpc(gen::METHOD_NET_REACH, &reach_body("127.0.0.1", 0, 100))
        .await
        .unwrap();
    assert_eq!(a.status, gen::ERR_PROTOCOL);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_net_reach_to_a_blackhole_times_out_in_bounded_time() {
    let r = start("blackhole");
    let s = raw(&r).await;
    // a documentation-range address nothing routes: either a timeout or an immediate
    // "unreachable" depending on the host's network, never a hang
    let t0 = Instant::now();
    let a = s
        .rpc(gen::METHOD_NET_REACH, &reach_body("203.0.113.1", 81, 300))
        .await
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(a.status, OK);
    let j: serde_json::Value = serde_json::from_slice(&untext(&a.body).body).unwrap();
    assert_eq!(j["ok"], false);
    if j["timed_out"] == true {
        assert_eq!(j["err"], "timed out");
        assert!(j["ms"].as_u64().unwrap() >= 250);
    }
}

// ---- the same methods through the engine's Rust transport ----

/// Runs `f` with the AVA1 transport registered for this thread (core's calls are blocking).
fn with_transport<T>(t: Arc<AvaTransport>, f: impl FnOnce() -> T) -> T {
    struct W(Arc<AvaTransport>);
    impl MgmtTransport for W {
        fn call(
            &self,
            addr: &str,
            method: Method,
            label: &str,
            body: &[u8],
            timeout: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            self.0.call(addr, method, label, body, timeout)
        }
    }
    tokio::task::block_in_place(|| {
        let _g = cmgmt::scoped_transport(Arc::new(W(t)));
        f()
    })
}

const CONSOLE: &str = "ps5-diag-console:9120";

#[tokio::test(flavor = "multi_thread")]
async fn every_diagnostics_method_works_through_the_rust_transport() {
    let r = start("core");
    mgmt::set_syslog(200_000, 0);
    let t = transport(&r);
    with_transport(t, || {
        // klog: an exact read comes back byte for byte
        assert_eq!(
            diagnostics::klog_read(CONSOLE, 2000).unwrap(),
            numbered_log(2000)
        );
        // klog at its 64 KiB ceiling fits one reply: whole, no note
        let k = diagnostics::klog_read(CONSOLE, 64 * 1024).unwrap();
        assert_eq!(k, numbered_log(64 * 1024));
        // a kernel buffer past one reply: the newest text, led by the transport's own note
        mgmt::set_syslog(1 << 20, 0);
        let s = hw::syslog_tail(CONSOLE).unwrap();
        assert!(s.starts_with(TAIL_CLIPPED));
        let body = &s[TAIL_CLIPPED.len()..];
        assert!(numbered_log(1 << 20).ends_with(body) && body.len() > 250_000);
        // and a short one is untouched
        mgmt::set_syslog(300, 0);
        assert_eq!(hw::syslog_tail(CONSOLE).unwrap(), numbered_log(300));
        // the sysctl error keeps the legacy `payload rejected SYSLOG_TAIL:` text
        mgmt::set_syslog(0, 1);
        let e = hw::syslog_tail(CONSOLE).unwrap_err().to_string();
        assert_eq!(
            e,
            "payload rejected SYSLOG_TAIL: syslog_tail_sysctl_errno_12"
        );
        mgmt::set_syslog(200_000, 0);
        // interfaces, modules, speed test
        let n = diagnostics::net_interfaces(CONSOLE).unwrap();
        assert_eq!(n.interfaces[0].name, "eth0");
        assert_eq!(n.interfaces[0].mtu, 1500);
        let m = diagnostics::proc_modules(CONSOLE, 0).unwrap();
        assert_eq!(m.modules[0].name, "libkernel.sprx");
        let sp = diagnostics::net_speed_test(CONSOLE, 7).unwrap();
        assert_eq!(sp.round_trips, 7);
        // reach: open, refused (an answer), malformed (an error)
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let open =
            diagnostics::net_reach(CONSOLE, "127.0.0.1", l.local_addr().unwrap().port(), 2000)
                .unwrap();
        assert!(open.ok);
        let shut = diagnostics::net_reach(CONSOLE, "127.0.0.1", closed_port(), 2000).unwrap();
        assert!(!shut.ok && !shut.timed_out && shut.errno != 0);
        let bad = diagnostics::net_reach(CONSOLE, "not-an-ip", 80, 100).unwrap_err();
        assert_eq!(bad.to_string(), "payload rejected NET_REACH: bad_address");
    });
}

// ---- bounded probes, reads while busy ----

#[tokio::test(flavor = "multi_thread")]
async fn sixteen_probes_at_once_never_put_more_than_six_in_flight_on_the_console() {
    let r = start("burst");
    mgmt::set_syslog(100, 3); // each holds its slot for 60 ms
    let t = transport(&r);
    let mut hs = Vec::new();
    for _ in 0..16 {
        let t = t.clone();
        hs.push(tokio::task::spawn_blocking(move || {
            t.call(
                CONSOLE,
                m::LOG_SYSLOG,
                "SYSLOG_TAIL",
                &[],
                Duration::from_secs(20),
            )
        }));
    }
    for h in hs {
        let reply = h.await.unwrap().unwrap().unwrap();
        assert_eq!(reply, numbered_log(100).into_bytes());
    }
    let peak = mgmt::diag_peak();
    assert!(peak >= 2, "the calls did overlap ({peak})");
    assert!(
        peak <= 6,
        "the engine gate holds the console to 6 general calls, saw {peak}"
    );
    mgmt::set_syslog(200_000, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_log_read_is_answered_while_other_reads_keep_the_console_busy() {
    let r = start("busy");
    mgmt::set_syslog(100, 3);
    let t = transport(&r);
    let mut hs = Vec::new();
    for _ in 0..4 {
        let t = t.clone();
        hs.push(tokio::task::spawn_blocking(move || {
            t.call(
                CONSOLE,
                m::LOG_SYSLOG,
                "SYSLOG_TAIL",
                &[],
                Duration::from_secs(20),
            )
        }));
    }
    tokio::time::sleep(Duration::from_millis(15)).await; // let them take their slots
    let t0 = Instant::now();
    let tk = t.clone();
    let k = tokio::task::spawn_blocking(move || {
        tk.call(
            CONSOLE,
            m::LOG_KLOG,
            "KLOG_READ",
            br#"{"max_bytes":500}"#,
            Duration::from_secs(5),
        )
    })
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(k, numbered_log(500).into_bytes());
    assert!(t0.elapsed() < Duration::from_secs(2));
    for h in hs {
        h.await.unwrap().unwrap().unwrap();
    }
    mgmt::set_syslog(200_000, 0);
}

// ---- the bug-report bundle, end to end ----

/// The console's files for the bundle test, served by the test (fs.list and fs.read are
/// Task 4's methods; their wire is covered there, this serves the same legacy bodies from a
/// host folder) in front of the real AVA1 transport, which carries every other method to the C server.
struct Routed {
    ava: Arc<AvaTransport>,
    root: PathBuf,
}

impl Routed {
    fn host_path(&self, console_path: &str) -> PathBuf {
        self.root.join(console_path.trim_start_matches('/'))
    }
}

impl MgmtTransport for Routed {
    fn call(
        &self,
        addr: &str,
        method: Method,
        label: &str,
        body: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let req: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        if method.id == m::FS_LIST.id {
            let p = self.host_path(req["path"].as_str().unwrap());
            let Ok(rd) = std::fs::read_dir(&p) else {
                return Err(cmgmt::MgmtError {
                    label: label.into(),
                    status: gen::ERR_IO,
                    cause: "fs_list_dir_failed".into(),
                }
                .into());
            };
            let mut entries = Vec::new();
            for e in rd.flatten() {
                let md = e.metadata().unwrap();
                entries.push(serde_json::json!({
                    "name": e.file_name().to_string_lossy(),
                    "kind": if md.is_dir() { "dir" } else { "file" },
                    "size": md.len(),
                    "mtime": 0,
                }));
            }
            let n = entries.len();
            return Ok(Some(
                serde_json::to_vec(&serde_json::json!({
                    "path": req["path"], "entries": entries, "truncated": false,
                    "total_scanned": n, "returned": n,
                }))
                .unwrap(),
            ));
        }
        if method.id == m::FS_READ.id {
            let data = std::fs::read(self.host_path(req["path"].as_str().unwrap()))?;
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let lim = req["limit"].as_u64().unwrap_or(u64::MAX) as usize;
            let end = data.len().min(off.saturating_add(lim));
            return Ok(Some(data[off.min(end)..end].to_vec()));
        }
        self.ava.call(addr, method, label, body, timeout)
    }
}

/// The event log is process-global C state: tests that set its path take turns.
static EVENTS: Mutex<()> = Mutex::new(());

const PREVIEW_CAP: usize = 256 * 1024;

/// Reads one log the way the client's collector does: from the start, or the last 256 KiB
/// when the listing says it is longer.
fn read_log(addr: &str, path: &str, size: usize) -> (String, bool) {
    let tail = size > PREVIEW_CAP;
    let (off, cap) = if tail {
        (size - PREVIEW_CAP, PREVIEW_CAP)
    } else {
        (0, PREVIEW_CAP)
    };
    let b = ps5upload_core::fs_ops::fs_read_with_timeout(
        addr,
        path,
        off as u64,
        cap as u64,
        Some(Duration::from_secs(10)),
        false,
    )
    .unwrap();
    (String::from_utf8_lossy(&b).into_owned(), tail)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bug_report_bundle_collects_logs_probes_and_job_events_from_the_c_server() {
    let r = start("bundle");
    mgmt::set_syslog(1 << 20, 0); // the handler's 1 MiB ceiling: more than a reply holds
                                  // the console's folders: a stderr.log that outgrew the cap, the previous instance's, a crash
                                  // marker, and the AVA1 event log written by the real C code
    let console = dir("bundle-console");
    let rt = console.join("data/ps5upload");
    std::fs::create_dir_all(rt.join("ava")).unwrap();
    let stderr = numbered_log(600_000);
    std::fs::write(rt.join("stderr.log"), &stderr).unwrap();
    std::fs::write(
        rt.join("stderr.log.old"),
        "[payload2] mgmt accept: errno 163\n",
    )
    .unwrap();
    std::fs::write(rt.join("crash.log"), "FATAL sig=11 frame=0x42\n").unwrap();
    let _ev = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    events::set(Some(&rt.join("ava/events.log")), 0);
    events::log("open job=0a0b0c0d kind=1 status=0 files=0/3 bytes=0/90 lanes=0");
    events::log("done job=0a0b0c0d kind=1 status=0 files=3/3 bytes=90/90 lanes=4");
    events::set(None, 0);
    drop(_ev);

    let routed = Arc::new(Routed {
        ava: transport(&r),
        root: console.to_path_buf(),
    });
    let bundle: Vec<(String, String)> = tokio::task::block_in_place(|| {
        let _g = cmgmt::scoped_transport(routed.clone());
        let mut out: Vec<(String, String)> = Vec::new();
        // 1. the black box first: list the folder, read what exists
        let list = ps5upload_core::fs_ops::list_dir(CONSOLE, "/data/ps5upload", Default::default())
            .unwrap();
        let size = |n: &str| {
            list.entries
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.size as usize)
        };
        for (name, leaf) in [
            ("stderr.log.old", "stderr_old.log"),
            ("crash.log", "crash.log"),
            ("stderr.log", "stderr.log"),
        ] {
            let sz = size(name).unwrap_or_else(|| panic!("{name} listed"));
            let (text, tail) = read_log(CONSOLE, &format!("/data/ps5upload/{name}"), sz);
            let text = if tail {
                format!("[earlier {} bytes omitted]\n{text}", sz - PREVIEW_CAP)
            } else {
                text
            };
            out.push((leaf.into(), text));
        }
        let ava =
            ps5upload_core::fs_ops::list_dir(CONSOLE, "/data/ps5upload/ava", Default::default())
                .unwrap();
        let sz = ava
            .entries
            .iter()
            .find(|e| e.name == "events.log")
            .unwrap()
            .size as usize;
        out.push((
            "ava_events.log".into(),
            read_log(CONSOLE, "/data/ps5upload/ava/events.log", sz).0,
        ));
        // 2. then the probes, one at a time, hardware-free ones first
        out.push((
            "net_interfaces".into(),
            serde_json::to_string(&diagnostics::net_interfaces(CONSOLE).unwrap()).unwrap(),
        ));
        out.push((
            "klog.txt".into(),
            diagnostics::klog_read(CONSOLE, 64 * 1024).unwrap(),
        ));
        out.push(("syslog.txt".into(), hw::syslog_tail(CONSOLE).unwrap()));
        out.push((
            "modules".into(),
            serde_json::to_string(&diagnostics::proc_modules(CONSOLE, 0).unwrap()).unwrap(),
        ));
        let reach = diagnostics::net_reach(CONSOLE, "127.0.0.1", closed_port(), 1000).unwrap();
        out.push(("reach".into(), serde_json::to_string(&reach).unwrap()));
        out
    });
    let get = |n: &str| {
        &bundle
            .iter()
            .find(|(k, _)| k == n)
            .unwrap_or_else(|| panic!("{n} in the bundle"))
            .1
    };
    // the long stderr.log arrives as its END (the newest lines), with the omission noted
    let st = get("stderr.log");
    assert!(st.starts_with(&format!(
        "[earlier {} bytes omitted]\n",
        600_000 - PREVIEW_CAP
    )));
    assert!(st.ends_with(&stderr[stderr.len() - 1000..]));
    assert_eq!(st.len(), PREVIEW_CAP + st.find('\n').unwrap() + 1);
    // what the previous instance said as it died is there
    assert!(get("stderr_old.log").contains("errno 163"));
    assert!(get("crash.log").contains("FATAL sig=11"));
    // the AVA1 event log replaces the tx logs: one line per job event, written by the C logger
    let ev = get("ava_events.log");
    let lines: Vec<&str> = ev.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("open job=0a0b0c0d kind=1 status=0 files=0/3 bytes=0/90 lanes=0"));
    assert!(lines[1].contains("done job=0a0b0c0d") && lines[1].contains("lanes=4"));
    // klog/syslog: clamped tails, marked
    assert_eq!(get("klog.txt"), &numbered_log(64 * 1024)); // fits one reply
    assert!(get("syslog.txt").starts_with(TAIL_CLIPPED)); // 1 MiB did not
    assert!(get("syslog.txt").len() > 250_000);
    assert!(get("net_interfaces").contains("192.168.1.50"));
    assert!(get("modules").contains("libkernel.sprx"));
    assert!(get("reach").contains(r#""ok":false"#));
    // and the console never ran two diagnostics at once for the whole collection
    let peak = mgmt::diag_peak();
    assert!(
        peak <= 1,
        "the bundle's probes ran one at a time, saw {peak}"
    );
}

// ---- the AVA1 event log (C) ----

#[test]
fn c_event_log_appends_timestamped_lines_and_is_off_without_a_path() {
    let _ev = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = dir("events");
    let p = d.join("events.log");
    events::set(None, 0);
    events::log("ignored: no path");
    assert!(!p.exists());
    events::set(Some(&p), 0);
    events::log("open job=aabbccdd kind=1 status=0 files=0/1 bytes=0/10 lanes=0");
    events::log(
        "fail job=aabbccdd kind=1 status=14 files=0/1 bytes=0/10 lanes=2 msg=\"disk full\"",
    );
    let t = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 2);
    // "YYYY-MM-DDTHH:MM:SSZ " then the line
    for l in &lines {
        let (stamp, rest) = l.split_at(21);
        assert_eq!(stamp.len(), 21);
        assert!(
            stamp.as_bytes()[4] == b'-' && stamp.ends_with("Z "),
            "{stamp}"
        );
        assert!(rest.contains("job=aabbccdd"));
    }
    // a runaway line is cut, never written whole
    events::log(&"x".repeat(5000));
    let t = std::fs::read_to_string(&p).unwrap();
    assert!(t.lines().last().unwrap().len() <= 21 + 512);
    events::set(None, 0);
}

#[test]
fn c_event_log_rolls_into_dot_old_keeping_the_newest_lines() {
    let _ev = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = dir("events-roll");
    let p = d.join("events.log");
    let old = d.join("events.log.old");
    events::set(Some(&p), 2000);
    for i in 0..100 {
        events::log(&format!(
            "open job={i:08x} kind=1 status=0 files=0/1 bytes=0/10 lanes=0"
        ));
    }
    events::set(None, 0);
    let (cur, prev) = (
        std::fs::read_to_string(&p).unwrap(),
        std::fs::read_to_string(&old).unwrap(),
    );
    assert!(
        cur.len() <= 2000 && prev.len() <= 2000,
        "{} {}",
        cur.len(),
        prev.len()
    );
    assert!(
        cur.lines().last().unwrap().contains("job=00000063"),
        "the newest line is in events.log"
    );
    // nothing is lost between the two files: they are consecutive runs of lines
    let all: Vec<&str> = prev.lines().chain(cur.lines()).collect();
    let ids: Vec<u32> = all
        .iter()
        .map(|l| {
            u32::from_str_radix(l.split("job=").nth(1).unwrap().get(..8).unwrap(), 16).unwrap()
        })
        .collect();
    assert!(
        ids.windows(2).all(|w| w[1] == w[0] + 1),
        "consecutive: {ids:?}"
    );
    assert_eq!(*ids.last().unwrap(), 99);
    // a roll never lands a half line at the start of a file
    assert!(cur.starts_with("20") && prev.starts_with("20"));
}

/// The job hooks (ava1_data.c, ava1_apply.c) write the log during a real upload to the C receiver.
// The log path is process-global, so the whole upload runs under the EVENTS lock on purpose.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn a_real_upload_leaves_open_and_done_lines_with_its_numbers() {
    // RIG first, then EVENTS (the bundle test's order): this test starts its own C server, which
    // must not run beside another test's.
    let _one_at_a_time = RIG.lock().unwrap_or_else(|e| e.into_inner());
    let _ev = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = dir("events-upload");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..5 {
        std::fs::write(src.join(format!("f{i}.bin")), vec![i as u8; 1000 + i]).unwrap();
    }
    let bytes: u64 = (0..5).map(|i| 1000 + i as u64).sum();
    let log = d.join("ava-events.log");
    events::set(Some(&log), 0);
    let peers = d.join("peers");
    let (me, mine) = {
        let me = Arc::new(Identity::generate().unwrap());
        PeerStore::load(&peers)
            .unwrap()
            .add(me.public(), "rust client")
            .unwrap();
        let mut mine = PeerStore::in_memory();
        mine.add(Identity::from_secret(SECRET).public(), "C data server")
            .unwrap();
        (me, Arc::new(Mutex::new(mine)))
    };
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let dest = d.join("dest");
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    let mut link = s.job([
        0x0a, 0x0b, 0x0c, 0x0d, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    ]);
    let source = ava1::source::LocalSource::new(src.clone());
    let m = ava1::manifest::walk(&source, &|_: &str| false).unwrap();
    let r = ava1::send::send_job(
        &mut link,
        Arc::new(m),
        Arc::new(source),
        ava1::send::SendOptions::upload(dest.to_str().unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 0);
    drop(srv); // joins the receiver: the done line is written before JobDone is sent
    events::set(None, 0);
    let t = std::fs::read_to_string(&log).unwrap();
    let open = t
        .lines()
        .find(|l| l.contains(" open job=0a0b0c0d"))
        .expect("an open line");
    assert!(open.contains("status=0"), "{open}");
    let done = t
        .lines()
        .find(|l| l.contains(" done job=0a0b0c0d"))
        .expect("a done line");
    assert!(
        done.contains("status=0") && done.contains("files=5/5"),
        "{done}"
    );
    assert!(done.contains(&format!("bytes={bytes}/{bytes}")), "{done}");
}

/// `AvaTransport::with_pool` takes a `&'static Pool`, so each test pool is leaked on purpose.
/// Keeping it in a static list makes it reachable for the process lifetime, so LeakSanitizer
/// does not report it (the sessions it holds live on the pool's own threads, beyond any
/// frame-name suppression). C leaks are still reported.
fn keep(p: &'static Pool) -> &'static Pool {
    static KEPT: std::sync::Mutex<Vec<&'static Pool>> = std::sync::Mutex::new(Vec::new());
    KEPT.lock().unwrap_or_else(|e| e.into_inner()).push(p);
    p
}
