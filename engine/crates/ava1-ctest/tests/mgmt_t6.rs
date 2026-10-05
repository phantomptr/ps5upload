#![cfg(unix)]
// The process-global shim state is guarded by a std Mutex held across the awaits on purpose.
#![allow(clippy::await_holding_lock)]
//! P3 Task 6: the apps / install / launch / process / media / search-index methods. The real
//! "P3 Task 6" rows of payload/src/mgmt_table.def (and app.launch, app.list, proc.process_list from
//! Task 2) run through the payload's real dispatcher over AVA1, with stub handlers that answer in
//! the shapes of the runtime.c handlers (csrc/mgmt_t6_shim.c). The Sony stubs take the payload's own
//! `sony_api_lock`. Static audits of runtime.c (lock, stack, recv) run here too.
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen::{self, MgmtText};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session, Timing};
use ava1::wire::Message;
use ava1_ctest::*;

const SECRET: [u8; 32] = [0x42; 32];
const OK: u16 = gen::STATUS_OK;

extern "C" {
    fn ava1_t6_install() -> i32;
    fn ava1_t6_uninstall();
    fn ava1_t6_entries(methods: *mut u16, flags: *mut u32, cap: usize) -> usize;
    fn ava1_t6_set_sony(use_lock: i32, hold_ms: i32);
    fn ava1_t6_peak() -> i32;
    fn ava1_t6_reset_peak();
    fn ava1_t6_set_list(n: u32, truncated: i32);
    fn ava1_t6_set_search_bytes(n: u32);
    fn ava1_t6_calls(method: u16) -> u32;
    fn ava1_t6_last_body(out: *mut u8, cap: usize) -> usize;
}

/// One test at a time: the shim's knobs and the installed table are process-global.
static ONE: Mutex<()> = Mutex::new(());

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(2000),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-t6-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn rig(tag: &str) -> (CServer, Session) {
    assert_eq!(unsafe { ava1_t6_install() }, 0);
    unsafe {
        ava1_t6_set_sony(1, 30);
        ava1_t6_set_list(3, 0);
        ava1_t6_set_search_bytes(200);
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
    (srv, s)
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

/// (status, text body or the cause as text, `more`)
async fn call(s: &Session, method: u16, body: &str) -> (u16, String, Option<u8>) {
    let r = s.rpc(method, &text(body)).await.unwrap();
    if r.status == OK {
        let t = untext(&r.body);
        (OK, String::from_utf8(t.body).unwrap(), t.more)
    } else {
        (r.status, String::from_utf8(r.body).unwrap(), None)
    }
}

fn last_body() -> String {
    let mut b = [0u8; 1024];
    let n = unsafe { ava1_t6_last_body(b.as_mut_ptr(), b.len()) };
    String::from_utf8_lossy(&b[..n]).into_owned()
}

fn payload() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../payload")
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

const REGISTER: u16 = gen::METHOD_APP_REGISTER;
const UNREGISTER: u16 = gen::METHOD_APP_UNREGISTER;
const LAUNCH: u16 = gen::METHOD_APP_LAUNCH;
const APP_LIST: u16 = gen::METHOD_APP_LIST;
const BROWSER: u16 = gen::METHOD_APP_LAUNCH_BROWSER;
const LIFECYCLE: u16 = gen::METHOD_APP_LIFECYCLE;
const INFO_QUERY: u16 = gen::METHOD_APP_INFO_QUERY;
const INFO_SET: u16 = gen::METHOD_APP_INFO_SET;
const DB_QUERY: u16 = gen::METHOD_APP_DB_QUERY;
const FOCUS: u16 = gen::METHOD_PROC_FOCUS;
const PROC_LIST: u16 = gen::METHOD_PROC_LIST;
const PROCESS_LIST: u16 = gen::METHOD_PROC_PROCESS_LIST;
const KILL: u16 = gen::METHOD_PROC_KILL;
const MODULES: u16 = gen::METHOD_PROC_MODULES;
const SAVES: u16 = gen::METHOD_SAVES_LIST;
const SHOTS: u16 = gen::METHOD_SHOTS_LIST;
const VIDEOS: u16 = gen::METHOD_VIDEOS_LIST;
const INDEX_START: u16 = gen::METHOD_INDEX_START;
const INDEX_STATUS: u16 = gen::METHOD_INDEX_STATUS;
const INDEX_SEARCH: u16 = gen::METHOD_INDEX_SEARCH;
const INDEX_CANCEL: u16 = gen::METHOD_INDEX_CANCEL;

// INFO_SET toasts through pop_notification, which takes sony_api_lock now (final review: console)
const SONY_METHODS: [u16; 7] = [
    REGISTER, UNREGISTER, LAUNCH, APP_LIST, BROWSER, LIFECYCLE, INFO_SET,
];

// ---- the real table and the static audits ----

#[test]
fn c_t6_table_names_every_method_and_flags_the_sony_ones() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let mut m = [0u16; 64];
    let mut f = [0u32; 64];
    let n = unsafe { ava1_t6_entries(m.as_mut_ptr(), f.as_mut_ptr(), 64) };
    let got: std::collections::BTreeMap<u16, u32> =
        m[..n].iter().copied().zip(f[..n].iter().copied()).collect();
    assert_eq!(got.len(), n, "no method twice");
    let all = [
        REGISTER,
        UNREGISTER,
        LAUNCH,
        APP_LIST,
        BROWSER,
        LIFECYCLE,
        INFO_QUERY,
        INFO_SET,
        DB_QUERY,
        FOCUS,
        PROC_LIST,
        PROCESS_LIST,
        KILL,
        MODULES,
        SAVES,
        SHOTS,
        VIDEOS,
        INDEX_START,
        INDEX_STATUS,
        INDEX_SEARCH,
        INDEX_CANCEL,
    ];
    for x in all {
        let fl = got
            .get(&x)
            .unwrap_or_else(|| panic!("method {x} not in the table"));
        // MGMT_SONY is bit 0 (payload/include/mgmt_rpc.h)
        assert_eq!(
            fl & 1 == 1,
            SONY_METHODS.contains(&x),
            "method {x}: MGMT_SONY flag"
        );
    }
    assert_eq!(n, all.len() + 2, "plus the shim's two test-only entries");
}

#[test]
fn c_t6_audits_lock_stack_recv_and_flags_are_clean() {
    // every MGMT_SONY entry of the Task 6 block reaches pthread_mutex_lock(&sony_api_lock), and no
    // runtime.c handler calls a p_sce* Sony function without it (the mutation test is by hand: see the report)
    audit("lock");
    audit("sony");
    audit("recv");
    audit("stack");
    audit("table");
}

// ---- each method, through the real dispatcher ----

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_app_register_unregister_launch_and_browser() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("apps").await;
    // register: the reply body is the legacy JSON, request reaches the handler unchanged
    let (st, body, more) = call(
        &s,
        REGISTER,
        r#"{"src_path":"/data/games/X","patch_drm_type":1}"#,
    )
    .await;
    assert_eq!((st, more), (OK, None));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["title_id"], "PPSA01234");
    assert_eq!(v["title_name"], "Stub \"Game\"");
    assert_eq!(v["used_nullfs"], true);
    assert_eq!(
        last_body(),
        r#"{"src_path":"/data/games/X","patch_drm_type":1}"#
    );
    // refusals keep the legacy token as the cause, with the closest status
    let (st, cause, _) = call(&s, REGISTER, "{}").await;
    assert_eq!(
        (st, cause.as_str()),
        (gen::ERR_PROTOCOL, "register_src_path_missing")
    );
    let (st, cause, _) = call(&s, REGISTER, r#"{"src_path":"/forbidden/x"}"#).await;
    assert_eq!(
        (st, cause.as_str()),
        (gen::ERR_PATH, "register_src_path_not_allowed")
    );
    // unregister: Sony's refusal code survives in the (successful) reply
    let (st, body, _) = call(&s, UNREGISTER, r#"{"title_id":"PPSA01234"}"#).await;
    assert_eq!((st, body.as_str()), (OK, r#"{"sony_uninstall_rc":0}"#));
    let (st, body, _) = call(&s, UNREGISTER, r#"{"title_id":"REFUSED"}"#).await;
    assert_eq!(
        (st, body.as_str()),
        (OK, r#"{"sony_uninstall_rc":2158631682}"#)
    );
    let (st, cause, _) = call(&s, UNREGISTER, "{}").await;
    assert_eq!(
        (st, cause.as_str()),
        (gen::ERR_PROTOCOL, "unregister_title_id_missing")
    );
    // launch: an empty acknowledgement; a failure carries the formatted reason
    let (st, body, _) = call(&s, LAUNCH, r#"{"title_id":"PPSA01234"}"#).await;
    assert_eq!((st, body.as_str()), (OK, ""));
    let (st, cause, _) = call(&s, LAUNCH, r#"{"title_id":"FAIL"}"#).await;
    assert_eq!(st, gen::ERR_INTERNAL);
    assert!(
        cause.starts_with("launch_all_strategies_failed: param=1"),
        "{cause}"
    );
    // browser
    let (st, body, _) = call(&s, BROWSER, "").await;
    assert_eq!((st, body.as_str()), (OK, ""));
    // an old-style empty request body (no MgmtText) is accepted for the no-argument methods
    let r = s.rpc(BROWSER, &[]).await.unwrap();
    assert_eq!(r.status, OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_failure_bodies_with_data_keep_their_fields_in_the_cause() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("kept").await;
    // proc.kill: errno and strerror text survive (the caller prints "No such process")
    let (st, cause, _) = call(&s, KILL, r#"{"pid":99999}"#).await;
    assert_eq!(st, gen::ERR_INTERNAL);
    let v: serde_json::Value = serde_json::from_str(&cause).expect("the cause is the whole body");
    assert_eq!(v["ok"], false);
    assert_eq!(v["errno"], 3);
    assert_eq!(v["reason"], "No such process");
    assert_eq!(v["err"], "kill_failed");
    assert_eq!(v["pid"], 99999);
    let (st, cause, _) = call(&s, KILL, r#"{"pid":1}"#).await;
    assert_eq!(st, gen::ERR_INTERNAL);
    assert!(cause.contains(r#""errno":1"#) && cause.contains("Operation not permitted"));
    let (st, body, _) = call(&s, KILL, r#"{"pid":4242}"#).await;
    assert_eq!((st, body.as_str()), (OK, r#"{"ok":true,"pid":4242}"#));
    // app.lifecycle: the Sony return code of a refused kill is in the cause
    let (st, cause, _) = call(&s, LIFECYCLE, r#"{"action":"kill","app_id":666}"#).await;
    assert_eq!(st, gen::ERR_INTERNAL);
    let v: serde_json::Value = serde_json::from_str(&cause).unwrap();
    assert_eq!(
        (v["ok"].clone(), v["code"].clone()),
        (false.into(), (-2146369530i64).into())
    );
    // a malformed request is the peer's error, and still carries the body
    let (st, cause, _) = call(&s, LIFECYCLE, "{}").await;
    assert_eq!(st, gen::ERR_PROTOCOL);
    assert_eq!(cause, r#"{"ok":false,"err":"bad_action"}"#);
    // and success is the plain body
    let (st, body, _) = call(&s, LIFECYCLE, r#"{"action":"list","app_id":0}"#).await;
    assert_eq!(st, OK);
    assert!(body.contains(r#""app_id":57368"#));
    // app.info_*: the `error` text of the legacy failure body travels whole
    let (st, cause, _) = call(&s, INFO_QUERY, r#"{"title_id":"","keys":""}"#).await;
    assert_eq!(st, gen::ERR_PROTOCOL);
    assert_eq!(cause, r#"{"ok":false,"error":"title_id is required"}"#);
    let (st, cause, _) = call(
        &s,
        INFO_SET,
        r#"{"title_id":"X","key":"k","val":"RUNNING"}"#,
    )
    .await;
    assert_eq!(st, gen::ERR_INTERNAL);
    assert_eq!(cause, r#"{"ok":false,"err":"title is running"}"#);
    let (st, body, _) = call(&s, INFO_SET, r#"{"title_id":"X","key":"k","val":"v"}"#).await;
    assert_eq!((st, body.as_str()), (OK, r#"{"ok":true,"err":null}"#));
    let (st, body, _) = call(&s, INFO_QUERY, r#"{"title_id":"CUSA00001","keys":""}"#).await;
    assert_eq!(st, OK);
    assert!(body.contains(r#""rows""#));
    // a failure body longer than the cause limit falls back to its token (never a cut-off JSON)
    // (covered at the unit level in the engine: legacy_body keeps both shapes)
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_processes_focus_database_and_modules() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("procs").await;
    for (m, key) in [
        (PROC_LIST, "procs"),
        (PROCESS_LIST, "procs"),
        (FOCUS, "big_app_id"),
        (DB_QUERY, "apps"),
        (MODULES, "modules"),
    ] {
        let (st, body, more) = call(&s, m, if m == MODULES { r#"{"pid":101}"# } else { "" }).await;
        assert_eq!((st, more), (OK, None), "method {m}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get(key).is_some(), "method {m}: {body}");
    }
    assert_eq!(last_body(), r#"{"pid":101}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_index_start_status_search_cancel() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("index").await;
    let (st, body, _) = call(&s, INDEX_STATUS, "").await;
    assert_eq!(st, OK);
    assert!(body.contains(r#""phase":"idle""#));
    let (st, body, _) = call(&s, INDEX_START, r#"{"roots":["/data","/user"]}"#).await;
    assert_eq!((st, body.as_str()), (OK, r#"{"started":true}"#));
    assert_eq!(last_body(), r#"{"roots":["/data","/user"]}"#);
    // "already building" is a normal reply (started:false), not an error status
    let (st, body, _) = call(&s, INDEX_START, "{}").await;
    assert_eq!(
        (st, body.as_str()),
        (OK, r#"{"started":false,"err":"already_building"}"#)
    );
    let (_, body, _) = call(&s, INDEX_STATUS, "").await;
    assert!(body.contains(r#""phase":"building""#));
    let (st, body, _) = call(&s, INDEX_SEARCH, r#"{"query":"*.pkg","limit":5}"#).await;
    assert_eq!(st, OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!v["results"].as_array().unwrap().is_empty());
    let (st, body, _) = call(&s, INDEX_CANCEL, "").await;
    assert_eq!((st, body.as_str()), (OK, r#"{"cancelled":true}"#));
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_a_search_reply_of_a_quarter_mebibyte_fits_and_one_byte_more_is_refused() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("search").await;
    // the real handler's buffer is 256 KiB and it stops 2.3 KiB short of it: such a reply passes whole
    let big = 256 * 1024 - 2300;
    unsafe { ava1_t6_set_search_bytes(big as u32) };
    let (st, body, more) = call(&s, INDEX_SEARCH, r#"{"query":"*","limit":5000}"#).await;
    assert_eq!((st, more), (OK, None));
    assert!(body.len() >= big && body.len() <= gen::RPC_TEXT_MAX as usize);
    let v: serde_json::Value = serde_json::from_str(&body).expect("complete JSON, not clipped");
    assert!(v["results"].as_array().unwrap().len() > 3000);
    // a reply over RPC_TEXT_MAX is ERR_INTERNAL "reply truncated", never a clipped OK
    unsafe { ava1_t6_set_search_bytes(gen::RPC_TEXT_MAX + 64) };
    let (st, cause, _) = call(&s, INDEX_SEARCH, "{}").await;
    assert_eq!((st, cause.as_str()), (gen::ERR_INTERNAL, "reply truncated"));
}

// ---- paging ----

/// Reads every page of a paged method: (all elements, the last page's text).
async fn pages(
    s: &Session,
    method: u16,
    extra: &str,
    limit: usize,
) -> (Vec<serde_json::Value>, String) {
    let mut all = Vec::new();
    let mut offset = 0usize;
    loop {
        let body = if limit == 0 {
            format!("{{{extra}\"offset\":{offset}}}")
        } else {
            format!("{{{extra}\"offset\":{offset},\"limit\":{limit}}}")
        };
        let (st, text, more) = call(s, method, &body).await;
        assert_eq!(st, OK, "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).expect("a page is valid JSON");
        let key = v
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, x)| x.is_array())
            .unwrap()
            .0
            .clone();
        let arr = v[&key].as_array().unwrap().clone();
        let n = arr.len();
        all.extend(arr);
        offset += n;
        if more == Some(0) || more.is_none() {
            return (all, text);
        }
        assert!(n > 0, "a page with `more` must carry an element");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_saves_shots_videos_and_app_list_page_cover_every_entry_once() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("paging").await;
    // 6,000 entries do not fit one 256 KiB reply: the default (no limit) window fills a reply, `more` says so
    unsafe { ava1_t6_set_list(6000, 0) };
    for (m, key, field) in [
        (SAVES, "saves", "title_id"),
        (SHOTS, "items", "path"),
        (VIDEOS, "items", "path"),
        (APP_LIST, "apps", "title_id"),
    ] {
        let (st, first, more) = call(&s, m, "{}").await;
        assert_eq!(st, OK, "method {m}");
        assert_eq!(
            more,
            Some(1),
            "method {m}: 6000 entries need more than one page"
        );
        assert!(first.len() <= gen::RPC_TEXT_MAX as usize);
        let (all, _) = pages(&s, m, "", 0).await;
        assert_eq!(all.len(), 6000, "method {m} ({key})");
        let mut seen = std::collections::BTreeSet::new();
        for e in &all {
            assert!(
                seen.insert(e[field].to_string()),
                "method {m}: duplicate {}",
                e[field]
            );
        }
        // explicit small pages cover the same entries, in order
        let (small, _) = pages(&s, m, "", 997).await;
        assert_eq!(small, all, "method {m}");
    }
    // quotes and brackets inside an entry do not confuse the pager
    unsafe { ava1_t6_set_list(5, 0) };
    let (all, _) = pages(&s, SHOTS, "", 2).await;
    assert_eq!(all.len(), 5);
    assert_eq!(
        all[3]["path"],
        "/user/av_contents/photo/1/3/shot \"3\" [x].jxr"
    );
    // the user_id filter reaches the handler beside the paging fields
    let _ = pages(&s, SAVES, "\"user_id\":7,", 0).await;
    assert!(last_body().contains("\"user_id\":7"));
    // a handler that reached its buffer says so, and the flag survives on the last page
    unsafe { ava1_t6_set_list(40, 1) };
    let (all, last) = pages(&s, SAVES, "", 15).await;
    assert_eq!(all.len(), 40);
    let v: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert_eq!(v["truncated"], true, "{last}");
    // empty, and a window past the end
    unsafe { ava1_t6_set_list(0, 0) };
    let (st, body, more) = call(&s, VIDEOS, "{}").await;
    assert_eq!((st, body.as_str(), more), (OK, r#"{"items":[]}"#, Some(0)));
    unsafe { ava1_t6_set_list(10, 0) };
    let (st, body, more) = call(&s, VIDEOS, r#"{"offset":99}"#).await;
    assert_eq!((st, body.as_str(), more), (OK, r#"{"items":[]}"#, Some(0)));
    // a bad window is the peer's error
    let (st, cause, _) = call(&s, SHOTS, r#"{"offset":-1}"#).await;
    assert_eq!((st, cause.as_str()), (gen::ERR_PROTOCOL, "bad offset"));
}

// ---- the Sony lock ----

/// `n` concurrent calls spread over the Sony methods; returns the peak number inside the stub's Sony section.
async fn storm(s: &Arc<Session>, n: usize) -> i32 {
    unsafe { ava1_t6_reset_peak() };
    let mut js = Vec::new();
    for i in 0..n {
        let s = s.clone();
        js.push(tokio::spawn(async move {
            let (m, b) = match i % 5 {
                0 => (LAUNCH, r#"{"title_id":"PPSA01234"}"#),
                1 => (REGISTER, r#"{"src_path":"/data/g"}"#),
                2 => (UNREGISTER, r#"{"title_id":"PPSA01234"}"#),
                3 => (LIFECYCLE, r#"{"action":"suspend","app_id":57368}"#),
                _ => (BROWSER, ""),
            };
            call(&s, m, b).await.0
        }));
    }
    for j in js {
        assert_eq!(j.await.unwrap(), OK);
    }
    unsafe { ava1_t6_peak() }
}

#[tokio::test(flavor = "multi_thread")]
async fn c_sony_methods_serialize_under_the_lock() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("serial").await;
    let s = Arc::new(s);
    // two concurrent app.launch calls never overlap in the Sony section...
    unsafe { ava1_t6_set_sony(1, 60) };
    unsafe { ava1_t6_reset_peak() };
    let (a, b) = {
        let (s1, s2) = (s.clone(), s.clone());
        tokio::join!(
            async move { call(&s1, LAUNCH, r#"{"title_id":"PPSA00001"}"#).await.0 },
            async move { call(&s2, LAUNCH, r#"{"title_id":"PPSA00002"}"#).await.0 }
        )
    };
    assert_eq!((a, b), (OK, OK));
    assert_eq!(
        unsafe { ava1_t6_peak() },
        1,
        "two launches ran inside the Sony lock at once"
    );
    // ...nor do six calls (the engine's per-console general limit) over five different Sony methods
    assert_eq!(storm(&s, 6).await, 1);
    // control: the same storm WITHOUT the lock does overlap, so the check can see an overlap
    unsafe { ava1_t6_set_sony(0, 60) };
    assert!(
        storm(&s, 6).await >= 2,
        "the unlocked control must overlap, or the test above proves nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_a_long_sony_call_does_not_block_other_methods() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("nonblock").await;
    let s = Arc::new(s);
    unsafe { ava1_t6_set_sony(1, 600) };
    let slow = {
        let s = s.clone();
        tokio::spawn(async move { call(&s, REGISTER, r#"{"src_path":"/data/g"}"#).await.0 })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (st, _, _) = call(&s, PROC_LIST, "").await;
    assert_eq!(st, OK);
    // A state check, not a wall-clock bound: proc.list answered while the 600 ms Sony call
    // was still running, so it did not wait behind it.
    assert!(!slow.is_finished(), "proc.list waited behind a Sony call");
    assert_eq!(slow.await.unwrap(), OK);
    assert_eq!(unsafe { ava1_t6_calls(REGISTER) }, 1);
}

#[test]
fn c_t6_uninstall_is_clean() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { ava1_t6_uninstall() };
}

// ---- the {"ok":false} rule (M3): the top-level key, wherever it sits ----

const ECHO: u16 = gen::METHOD_FS_MOUNT_PKG;
const PROGRESS: u16 = gen::METHOD_FS_MOUNT;

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_legacy_ok_false_is_found_wherever_the_top_level_key_sits() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("okfalse").await;
    // (body, expected status, expected cause when an error)
    let cases: &[(&str, u16, &str)] = &[
        // ok first, ok last, ok in the middle: all failures, the whole body is the cause
        (
            r#"{"ok":false,"err":"launch_failed"}"#,
            gen::ERR_INTERNAL,
            r#"{"ok":false,"err":"launch_failed"}"#,
        ),
        (
            r#"{"err":"launch_failed","ok":false}"#,
            gen::ERR_INTERNAL,
            r#"{"err":"launch_failed","ok":false}"#,
        ),
        (
            r#"{"a":1,"ok":false,"b":2}"#,
            gen::ERR_INTERNAL,
            r#"{"a":1,"ok":false,"b":2}"#,
        ),
        (
            r#"{ "ok" : false , "err":"no_space_left"}"#,
            gen::ERR_NO_SPACE,
            r#"{ "ok" : false , "err":"no_space_left"}"#,
        ),
        // the token still maps the status; the token comes from the top level only
        (
            r#"{"detail":{"err":"exists"},"ok":false,"err":"x"}"#,
            gen::ERR_INTERNAL,
            r#"{"detail":{"err":"exists"},"ok":false,"err":"x"}"#,
        ),
        // data that callers read stays in the cause (handle_process_kill)
        (
            r#"{"err":"kill_failed","errno":3,"reason":"No such process","pid":9,"ok":false}"#,
            gen::ERR_INTERNAL,
            r#"{"err":"kill_failed","errno":3,"reason":"No such process","pid":9,"ok":false}"#,
        ),
    ];
    for (body, status, cause) in cases {
        let (st, got, _) = call(&s, ECHO, body).await;
        assert_eq!((st, got.as_str()), (*status, *cause), "{body}");
    }
    // NOT failures: ok true, no ok, ok nested in an object or an array, "ok:false" inside a string value,
    // a key that merely contains ok, a string value "false"
    for body in [
        r#"{"ok":true}"#,
        r#"{"err":"x","ok":true}"#,
        r#"{"apps":[]}"#,
        r#"{"detail":{"ok":false},"n":1}"#,
        r#"{"list":[{"ok":false}],"n":1}"#,
        r#"{"note":"x \"ok\":false y","ok":true}"#,
        r#"{"note":"{\"ok\":false}"}"#,
        r#"{"token":"ok:false"}"#,
        r#"{"look":false,"ok_count":false}"#,
        r#"{"ok":"false"}"#,
        r#"{"started":false,"err":"already_building"}"#,
    ] {
        let (st, got, _) = call(&s, ECHO, body).await;
        assert_eq!(
            (st, got.as_str()),
            (OK, body),
            "must stay a success: {body}"
        );
    }
    // not an object at all (a list, plain text) is a success body, never a failure
    for body in ["[]", "ok:false", "model=PS5\nok=false\n"] {
        let (st, got, _) = call(&s, ECHO, body).await;
        assert_eq!((st, got.as_str()), (OK, body));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_a_progress_frame_then_a_result_reports_the_final_frame() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("multiframe").await;
    // the last frame is the answer: the success...
    let (st, body, _) = call(&s, PROGRESS, "{}").await;
    assert_eq!((st, body.as_str()), (OK, r#"{"result":1,"ok":true}"#));
    // ...and a failing result after a progress frame is an error, not the progress frame's "success"
    let (st, cause, _) = call(&s, PROGRESS, "FAIL").await;
    assert_eq!(st, gen::ERR_INTERNAL);
    assert_eq!(cause, r#"{"result":1,"ok":false,"err":"final_failed"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_a_failure_body_too_long_for_a_cause_travels_as_its_token() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("longfail").await;
    let long = format!(
        r#"{{"ok":false,"err":"no_space","pad":"{}"}}"#,
        "x".repeat(300)
    );
    let (st, cause, _) = call(&s, ECHO, &long).await;
    assert_eq!((st, cause.as_str()), (gen::ERR_NO_SPACE, "no_space"));
}

#[tokio::test(flavor = "multi_thread")]
async fn c_t6_a_request_with_an_embedded_nul_is_refused_not_truncated() {
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (_srv, s) = rig("nul").await;
    let r = s
        .rpc(ECHO, &text("{\"a\":1}\0{\"ok\":false}"))
        .await
        .unwrap();
    assert_eq!(r.status, gen::ERR_PROTOCOL);
    assert_eq!(r.body, b"request contains NUL");
    // the same through a typed-path-free method that does reach the handler: an ordinary body still works
    assert_eq!(call(&s, ECHO, "{}").await.0, OK);
}
