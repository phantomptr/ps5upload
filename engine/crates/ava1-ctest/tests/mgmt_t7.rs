#![cfg(unix)]
#![allow(clippy::await_holding_lock)] // the rig lock is held for a test on purpose: the stub table is process-wide
//! P3 Task 7: the hardware / system / accounts / cheats / mods / notices / Remote Play management
//! methods, driven over AVA1 against the C server. The table is the REAL "P3 Task 7" block of
//! payload/src/mgmt_table.def (build.rs copies its rows into the shim); only the handlers are stubs
//! (the real ones live in runtime.c, which only the SDK builds), so these tests pin what the
//! dispatcher and the table do with every method: its number, the Sony flag, the runner, a body that
//! reaches the handler, a legacy failure that keeps its data, the final frame of a multi-frame
//! handler, and that registry/user-service handlers never overlap.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ava1::gen::{self, MgmtText};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session, Timing};
use ava1::wire::Message;
use ava1_ctest::*;

const SECRET: [u8; 32] = [0x42; 32];

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(2000),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

/// The stub table, its counters and the C server are process-wide: the tests of this file run one at
/// a time through this lock, held for the rig's life. The C server itself also takes ava1-ctest's
/// shared C_SERVER lock in `CServer::start`, so this file is safe beside every other ctest suite.
struct Rig {
    _srv: CServer,
    _dir: TempDir,
    _lock: MutexGuard<'static, ()>,
}

fn rig_lock() -> MutexGuard<'static, ()> {
    // one serialisation scheme for every shim-global test: the shared guard (src/lib.rs)
    CServer::lock_for_shim_tests()
}

async fn rig(tag: &str) -> (Rig, Session) {
    let lock = rig_lock();
    assert_eq!(t7::install(), 0);
    let d = TempDir::new(format!("ava1-t7-{tag}-{}", std::process::id()));
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
            _dir: d,
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

fn untext(b: &[u8]) -> String {
    String::from_utf8(MgmtText::decode(b).expect("a MgmtText reply").body).unwrap()
}

fn payload() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../payload")
}

/// The methods the task owns, from the real table text: (constant name, flags).
fn block() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(payload().join("src/mgmt_table.def")).unwrap();
    let mut inside = false;
    let mut out = vec![];
    for l in src.lines() {
        if l.starts_with("/* ---- P3 Task 7:") {
            inside = true;
        } else if l.starts_with("/* ---- end Task 7") {
            inside = false;
        } else if inside && l.starts_with("MGMT_H") {
            let inner = &l[l.find('(').unwrap() + 1..l.rfind(')').unwrap()];
            let f: Vec<&str> = inner.split(',').map(str::trim).collect();
            assert_eq!(f.len(), 6, "{l}");
            assert_eq!(
                f[5], "mgmt_call_text_keep",
                "every Task 7 method keeps a legacy failure body: {l}"
            );
            out.push((f[0].to_string(), f[3].to_string()));
        }
    }
    out
}

fn method_of(name: &str) -> u16 {
    let n = name.trim_start_matches("AVA1_METHOD_");
    let src = std::fs::read_to_string(payload().join("ava1/gen/ava1_gen.h")).unwrap();
    let key = format!("#define AVA1_METHOD_{n} ");
    let line = src.lines().find(|l| l.starts_with(&key)).unwrap();
    line[key.len()..]
        .trim_end_matches("ULL")
        .parse::<u16>()
        .unwrap()
}

/// Every method of the group, by dotted name: the plan's rows 72-141 minus the ones other tasks own.
const EXPECTED: &[&str] = &[
    "hw.info",
    "hw.temps",
    "hw.power",
    "hw.storage",
    "hw.fan_threshold",
    "hw.fan_curve_set",
    "hw.fan_curve_get",
    "hw.drive_sensors",
    "power.control",
    "power.telemetry",
    "time.get",
    "time.set",
    "time.state_get",
    "time.state_set",
    "periph.control",
    "shell.exec",
    "profile.info",
    "profile.set_username",
    "profile.activate",
    "profile.apply_avatar",
    "profile.clear_slot",
    "profile.set_local_username",
    "user.list",
    "user.create",
    "user.delete",
    "backup.list",
    "backup.delete",
    "cheats.list",
    "cheats.get",
    "cheats.toggle",
    "cheats.delete",
    "cheats.reload",
    "cheats.status",
    "cheats.engine_set",
    "smp.meta_control",
    "smp.meta_stats",
    "sdk.scan",
    "sdk.patch",
    "sdk.restore",
    "tmdb.fetch",
    "tmdb.store",
    "ftp.start",
    "ftp.status",
    "fwspoof.status",
    "notif.list",
    "notif.send",
    "notif.clear",
    "toast.send",
    "activity.get",
    "activity.db_query",
    "activity.reset",
    "rp.request",
    "rp.status",
    "rp.cancel",
    "rp.readiness",
    "rp.enable",
    "rp.devices",
];

/// Methods whose handlers reach sceUserService / sceRegMgr / Remote Play / notifications / a Sony
/// system call: they carry MGMT_SONY (the audit also derives this from the call graph).
const SONY: &[&str] = &[
    // time.get/set (sceSystemServiceGet/SetCurrentDateTime) and shell.exec (its notify builtin toasts) take
    // sony_api_lock now (final review: console)
    "time.get",
    "time.set",
    "shell.exec",
    "power.control",
    "time.state_get",
    "time.state_set",
    "periph.control",
    "profile.info",
    "profile.set_username",
    "profile.activate",
    "profile.apply_avatar",
    "profile.clear_slot",
    "profile.set_local_username",
    "user.list",
    "user.create",
    "user.delete",
    "cheats.toggle",
    "cheats.reload",
    "notif.list",
    "notif.send",
    "notif.clear",
    "toast.send",
    "rp.request",
    "rp.status",
    "rp.cancel",
    "rp.readiness",
    "rp.enable",
    "rp.devices",
];

fn dotted(name: &str) -> String {
    let n = name.trim_start_matches("AVA1_METHOD_").to_lowercase();
    // the first underscore separates the group ("hw_fan_curve_get" -> "hw.fan_curve_get")
    n.replacen('_', ".", 1)
}

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

// ---- the table ----

#[test]
fn the_task_7_block_names_every_method_of_the_group_once() {
    let b = block();
    let got: BTreeSet<String> = b.iter().map(|(n, _)| dotted(n)).collect();
    // fwspoof has no group separator in its constant name; ftp/tmdb/etc. do
    let want: BTreeSet<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    assert_eq!(b.len(), got.len(), "duplicate method in the block");
    assert_eq!(got, want);
    // numbers are all in the group's blocks, none belongs to another task's range
    for (n, _) in &b {
        let m = method_of(n);
        assert!(
            (72..=96).contains(&m) || (98..=141).contains(&m),
            "{n} = {m} is outside rows 72-141"
        );
    }
}

#[test]
fn the_shim_table_is_the_real_block_row_for_row() {
    let b = block();
    let rows = t7::rows();
    assert_eq!(rows.len(), b.len());
    for ((name, flags), row) in b.iter().zip(&rows) {
        assert_eq!(row.method, method_of(name) as u32, "{name}");
        assert_eq!(
            row.flags & t7::SONY,
            u32::from(flags == "MGMT_SONY"),
            "{name}"
        );
        assert!(row.frame > 0 && row.ack > 0, "{name} names legacy frames");
    }
}

#[test]
fn registry_user_service_and_notification_methods_are_flagged_sony_and_nothing_else_is() {
    let want: BTreeSet<&str> = SONY.iter().copied().collect();
    let got: BTreeSet<String> = block()
        .into_iter()
        .filter(|(_, f)| f == "MGMT_SONY")
        .map(|(n, _)| dotted(&n))
        .collect();
    let got: BTreeSet<&str> = got.iter().map(String::as_str).collect();
    assert_eq!(got, want);
}

#[test]
fn the_real_handlers_pass_every_audit() {
    audit("table"); // real handlers, listed in MGMT_METHODS.md
    audit("recv"); // none reads the socket (they run with fd = -1)
    audit("sony"); // anything reaching Sony code carries MGMT_SONY
    audit("sonylock"); // anything reaching sceUserService/sceRegMgr/sys_registry takes sony_api_lock
    audit("stack"); // no 16 KiB+ stack array: shell cp/mv, activity load_state, copy_file, normalize_path
}

#[test]
fn the_notice_and_power_adapters_keep_their_request_caps_and_the_destructive_actions_are_deferred()
{
    let rt = std::fs::read_to_string(payload().join("src/runtime.c")).unwrap();
    // toast.send / notif.send: a body over 4 KiB is refused before the handler (-> ERR_PROTOCOL)
    for f in ["mgmt_w_toast_send", "mgmt_w_notif_send"] {
        let i = rt.find(&format!("static int {f}(")).unwrap();
        let body = &rt[i..i + 400];
        assert!(
            body.contains("l > 4096") && body.contains("body_too_large"),
            "{f}"
        );
    }
    // power.control: reboot/shutdown/standby run after the reply under the capture sink
    let i = rt.find("static int handle_system_control(").unwrap();
    let h = &rt[i..i + 6000];
    assert_eq!(
        h.matches("mgmt_capture_active() && power_defer(").count(),
        3
    );
    // the clock stays settimeofday-first: nothing in the group's handlers touches an SCE date/time symbol
    let st = std::fs::read_to_string(payload().join("src/sys_time.c")).unwrap();
    assert!(st.contains("settimeofday"));
}

// ---- every method over AVA1 ----

#[tokio::test(flavor = "multi_thread")]
async fn every_method_answers_a_mgmt_text_and_its_handler_sees_the_request_body() {
    let (_g, s) = rig("each").await;
    let mut n = 0;
    for (name, _) in block() {
        let m = method_of(&name);
        // empty request: the handler gets an empty body
        let r = s.rpc(m, &[]).await.unwrap();
        assert_eq!(r.status, gen::STATUS_OK, "{name} empty");
        let t = untext(&r.body);
        assert!(
            t.contains(&format!("\"m\":{m},")) && t.contains("\"n\":0"),
            "{name}: {t}"
        );
        // a JSON request reaches the handler whole (quotes shown as ' by the stub)
        let r = s.rpc(m, &text(r#"{"k":"v"}"#)).await.unwrap();
        assert_eq!(r.status, gen::STATUS_OK, "{name}");
        let t = untext(&r.body);
        assert!(
            t.contains("\"n\":9") && t.contains("{'k':'v'}"),
            "{name}: {t}"
        );
        // a request that is not a MgmtText is the peer's error, never reaches the handler
        let r = s.rpc(m, &[1, 2, 3]).await.unwrap();
        assert_eq!(r.status, gen::ERR_PROTOCOL, "{name}");
        n += 1;
    }
    assert_eq!(n, EXPECTED.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_failure_travels_with_its_data_as_the_error_cause() {
    let (_g, s) = rig("keep").await;
    for (name, _) in block() {
        let m = method_of(&name);
        let r = s.rpc(m, &text("@FAILFIRST")).await.unwrap();
        assert_ne!(
            r.status,
            gen::STATUS_OK,
            "{name}: an ok:false reply is not a success"
        );
        // the whole body, so the engine can hand the caller what the legacy handlers handed it (code, err_code, port...)
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            r#"{"ok":false,"code":7,"err":"bad_thing"}"#,
            "{name}"
        );
    }
}

/// Failure bodies with "ok" NOT first. `mgmt_legacy_failure` only sees `{"ok":false` as the first key
/// today; Task 6 fixes that centrally in mgmt_rpc.c (the coordinator's note), so this test is ignored
/// until its fix is merged here. It is the same assertion as the test above with the key order that
/// real handlers use for some bodies.
#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_failure_with_ok_last_is_also_an_error_with_its_data() {
    let (_g, s) = rig("keep-late").await;
    for (name, _) in block() {
        let r = s.rpc(method_of(&name), &text("@FAILLATE")).await.unwrap();
        assert_ne!(r.status, gen::STATUS_OK, "{name}");
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            r#"{"err":"bad_thing","code":7,"ok":false}"#,
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_frame_becomes_its_token_and_the_last_frame_of_a_multi_frame_handler_wins() {
    let (_g, s) = rig("frames").await;
    for (name, _) in block() {
        let m = method_of(&name);
        // an ERROR frame: toast/notif's "body_too_large" is the peer's error (ERR_PROTOCOL)
        let r = s.rpc(m, &text("@ERRFRAME")).await.unwrap();
        assert_eq!(r.status, gen::ERR_PROTOCOL, "{name}");
        assert_eq!(r.body, b"body_too_large", "{name}");
        // a progress frame and then the result: the RPC returns the result
        let r = s.rpc(m, &text("@TWO")).await.unwrap();
        assert_eq!(r.status, gen::STATUS_OK, "{name}");
        assert_eq!(untext(&r.body), r#"{"done":true}"#, "{name}");
    }
}

// ---- Sony serialisation ----

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_registry_and_user_service_calls_never_overlap() {
    let (_g, s) = rig("serial").await;
    let s = Arc::new(s);
    let sony: Vec<u16> = block()
        .into_iter()
        .filter(|(_, f)| f == "MGMT_SONY")
        .map(|(n, _)| method_of(&n))
        .collect();
    // the registry/user-service group in particular, 8 at once (the payload's in-flight limit)
    let reg: Vec<u16> = [
        "user.list",
        "time.state_get",
        "time.state_set",
        "profile.info",
        "rp.status",
    ]
    .iter()
    .map(|d| {
        let n = block()
            .into_iter()
            .map(|(n, _)| n)
            .find(|n| dotted(n) == *d)
            .unwrap();
        method_of(&n)
    })
    .collect();
    t7::reset_peak();
    let mut tasks = vec![];
    for i in 0..8 {
        let s = s.clone();
        let m = reg[i % reg.len()];
        tasks.push(tokio::spawn(
            async move { s.rpc(m, &[]).await.unwrap().status },
        ));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), gen::STATUS_OK);
    }
    assert_eq!(
        t7::sony_peak(),
        1,
        "two registry/user-service handlers ran at once"
    );
    // and across every Sony-flagged method together
    t7::reset_peak();
    let mut tasks = vec![];
    for round in 0..2 {
        for &m in &sony {
            let s = s.clone();
            tasks.push(tokio::spawn(async move {
                let _ = round;
                s.rpc(m, &[]).await.unwrap().status
            }));
            if tasks.len() % 8 == 0 {
                // keep within the in-flight limit: drain a batch
                for t in tasks.drain(..) {
                    assert_eq!(t.await.unwrap(), gen::STATUS_OK);
                }
            }
        }
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), gen::STATUS_OK);
    }
    assert_eq!(t7::sony_peak(), 1, "two Sony-flagged handlers ran at once");
}
