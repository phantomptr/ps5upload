//! P3 Task 6 over the real transport: paged lists, the Sony-lock sub-limit, ERR_BUSY retry of a
//! launch, and the core call sites (register, kill) end to end against the Rust AVA1 server.
//! Public API only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::gen::{self, MgmtText};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::mgmt::{is_sony_long, AvaTransport, MgmtGate, GENERAL, SONY_LONG};
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt::{self, m, MgmtError, MgmtTransport};

const T: Duration = Duration::from_secs(20);

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-t6-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn text_more(s: &str, more: bool) -> RpcReply {
    RpcReply {
        status: gen::STATUS_OK,
        body: MgmtText {
            body: s.as_bytes().to_vec(),
            more: Some(u8::from(more)),
        }
        .to_bytes()
        .unwrap(),
    }
}

fn text(s: &str) -> RpcReply {
    RpcReply {
        status: gen::STATUS_OK,
        body: MgmtText {
            body: s.as_bytes().to_vec(),
            more: None,
        }
        .to_bytes()
        .unwrap(),
    }
}

fn err(status: u16, cause: &str) -> RpcReply {
    RpcReply {
        status,
        body: cause.as_bytes().to_vec(),
    }
}

fn text_of(req: &[u8]) -> String {
    String::from_utf8(MgmtText::decode(req).unwrap().body).unwrap()
}

async fn console(tag: &str, handler: RpcHandler) -> (Arc<AvaTransport>, String) {
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
    (Arc::new(t), format!("{tag}-t6"))
}

async fn call(
    t: &Arc<AvaTransport>,
    addr: &str,
    method: mgmt::Method,
    label: &str,
    body: &[u8],
) -> anyhow::Result<Option<Vec<u8>>> {
    let (t, addr, label, body) = (
        t.clone(),
        addr.to_string(),
        label.to_string(),
        body.to_vec(),
    );
    tokio::task::spawn_blocking(move || t.call(&addr, method, &label, &body, T))
        .await
        .unwrap()
}

// ---- paging ----

/// A console whose `method` list has `total` entries (`{"<key>":[{"i":n},..]}`), `page` per reply.
fn paged_handler(
    method: u16,
    key: &'static str,
    total: usize,
    page: usize,
    seen: Arc<Mutex<Vec<String>>>,
    truncated: bool,
) -> RpcHandler {
    Box::new(move |m, body| {
        assert_eq!(m, method);
        let req = text_of(body);
        seen.lock().unwrap().push(req.clone());
        let v: serde_json::Value = serde_json::from_str(&req).unwrap();
        let off = v["offset"].as_u64().unwrap() as usize;
        let end = (off + page).min(total);
        let items: Vec<String> = (off..end).map(|i| format!("{{\"i\":{i}}}")).collect();
        let more = end < total;
        let tail = if truncated && !more {
            ",\"truncated\":true"
        } else {
            ""
        };
        text_more(&format!("{{\"{key}\":[{}]{tail}}}", items.join(",")), more)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn app_list_pages_are_read_to_the_end_and_merged_into_one_document() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (t, c) = console(
        "applist",
        paged_handler(
            gen::METHOD_APP_LIST,
            "apps",
            2500,
            1000,
            seen.clone(),
            false,
        ),
    )
    .await;
    let body = call(&t, &c, m::APP_LIST, "APP_LIST_REGISTERED", b"")
        .await
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let apps = v["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 2500);
    for (i, a) in apps.iter().enumerate() {
        assert_eq!(a["i"], i, "in order, each once");
    }
    assert!(v.get("more").is_none() && v.get("truncated").is_none());
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .map(
                |s| serde_json::from_str::<serde_json::Value>(s).unwrap()["offset"]
                    .as_u64()
                    .unwrap()
            )
            .collect::<Vec<_>>(),
        vec![0, 1000, 2000],
        "one request per page, each at the entries already read"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_pages_keep_the_user_filter_and_the_truncated_mark() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (t, c) = console(
        "saves",
        paged_handler(gen::METHOD_SAVES_LIST, "saves", 25, 10, seen.clone(), true),
    )
    .await;
    let body = call(&t, &c, m::SAVES_LIST, "LIST_SAVES", br#"{"user_id":7}"#)
        .await
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["saves"].as_array().unwrap().len(), 25);
    assert_eq!(v["truncated"], true);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for s in seen.iter() {
        let r: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(
            r["user_id"], 7,
            "the legacy request rides beside the window"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shots_and_videos_use_their_own_methods_and_an_empty_list_is_one_call() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (t, c) = console(
        "shots",
        paged_handler(gen::METHOD_SHOTS_LIST, "items", 0, 10, seen.clone(), false),
    )
    .await;
    let body = call(&t, &c, m::SHOTS_LIST, "LIST_SCREENSHOTS", b"")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body, br#"{"items":[]}"#);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_that_says_more_but_carries_nothing_is_an_error_not_a_loop() {
    let (t, c) = console("stuck", Box::new(|_, _| text_more(r#"{"apps":[]}"#, true))).await;
    let e = call(&t, &c, m::APP_LIST, "APP_LIST_REGISTERED", b"")
        .await
        .unwrap_err();
    let me = e.downcast_ref::<MgmtError>().expect("a MgmtError");
    assert_eq!(me.cause, "reply_paged_empty_page");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_paged_method_with_more_set_is_still_refused() {
    let (t, c) = console("notpaged", Box::new(|_, _| text_more("{}", true))).await;
    let e = call(&t, &c, m::PROC_LIST, "PROC_LIST", b"")
        .await
        .unwrap_err();
    assert_eq!(e.downcast_ref::<MgmtError>().unwrap().cause, "reply_paged");
}

// ---- the Sony sub-limit and BUSY ----

#[test]
fn only_the_long_sony_lock_methods_take_the_sub_limit() {
    for id in [
        gen::METHOD_APP_REGISTER,
        gen::METHOD_APP_UNREGISTER,
        gen::METHOD_APP_LAUNCH,
    ] {
        assert!(is_sony_long(id));
    }
    for id in [
        gen::METHOD_APP_LIST,
        gen::METHOD_APP_LIFECYCLE,
        gen::METHOD_PROC_KILL,
        gen::METHOD_NODE_STATUS,
        gen::METHOD_JOB_CANCEL,
        gen::METHOD_HW_INFO,
    ] {
        assert!(!is_sony_long(id));
    }
    assert_eq!(SONY_LONG, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_third_long_sony_call_waits_and_never_takes_a_third_slot() {
    let gate = MgmtGate::default();
    let a = gate.acquire_for(gen::METHOD_APP_LAUNCH).await;
    let b = gate.acquire_for(gen::METHOD_APP_REGISTER).await;
    assert_eq!(gate.available().0, GENERAL - 2);
    // a third long call parks on the sub-limit, holding no slot while it waits
    let third = tokio::time::timeout(
        Duration::from_millis(150),
        gate.acquire_for(gen::METHOD_APP_UNREGISTER),
    )
    .await;
    assert!(third.is_err(), "the third long Sony call must wait");
    assert_eq!(
        gate.available().0,
        GENERAL - 2,
        "a waiting call holds no slot"
    );
    // other methods still get slots
    let other = tokio::time::timeout(
        Duration::from_millis(150),
        gate.acquire_for(gen::METHOD_PROC_LIST),
    )
    .await
    .expect("a non-Sony call is not held up");
    // and the priority methods keep working
    let prio = gate.acquire_for(gen::METHOD_NODE_STATUS).await;
    drop((a, other, prio));
    let third = tokio::time::timeout(
        Duration::from_millis(500),
        gate.acquire_for(gen::METHOD_APP_UNREGISTER),
    )
    .await
    .expect("freed once one long call finished");
    drop((b, third));
    assert_eq!(gate.available(), (GENERAL, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn six_launches_put_at_most_two_in_flight_and_other_calls_still_answer() {
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (i2, p2) = (inflight.clone(), peak.clone());
    let (t, c) = console(
        "sonylong",
        Box::new(move |method, _| match method {
            gen::METHOD_APP_LAUNCH => {
                let now = i2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                i2.fetch_sub(1, Ordering::SeqCst);
                text("")
            }
            _ => text("{\"procs\":[]}"),
        }),
    )
    .await;
    let launches: Vec<_> = (0..6)
        .map(|_| {
            let (t, c) = (t.clone(), c.clone());
            tokio::spawn(async move {
                call(
                    &t,
                    &c,
                    m::APP_LAUNCH,
                    "APP_LAUNCH",
                    br#"{"title_id":"PPSA00001"}"#,
                )
                .await
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(80)).await;
    // while two launches run, an unrelated call answers at once
    let t0 = Instant::now();
    call(&t, &c, m::PROC_LIST, "PROC_LIST", b"")
        .await
        .unwrap()
        .unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(120),
        "{:?}",
        t0.elapsed()
    );
    for l in launches {
        l.await.unwrap().unwrap().unwrap();
    }
    assert!(
        peak.load(Ordering::SeqCst) <= SONY_LONG,
        "the console saw {} launches at once",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_busy_is_retried_by_the_engine() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let (t, c) = console(
        "launchbusy",
        Box::new(move |method, _| {
            assert_eq!(method, gen::METHOD_APP_LAUNCH);
            if n2.fetch_add(1, Ordering::SeqCst) < 2 {
                err(gen::ERR_BUSY, "launch_in_progress")
            } else {
                text("")
            }
        }),
    )
    .await;
    let r = call(
        &t,
        &c,
        m::APP_LAUNCH,
        "APP_LAUNCH",
        br#"{"title_id":"PPSA00001"}"#,
    )
    .await
    .unwrap();
    assert_eq!(r.unwrap(), b"");
    assert_eq!(
        n.load(Ordering::SeqCst),
        3,
        "two BUSY answers, then the launch"
    );
}

// ---- the core call sites over the real transport ----

#[tokio::test(flavor = "multi_thread")]
async fn install_register_step_uses_app_register_over_ava1() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let (t, c) = console(
        "register",
        Box::new(move |method, body| {
            s2.lock().unwrap().push((method, text_of(body)));
            match method {
                gen::METHOD_APP_REGISTER => {
                    text(r#"{"title_id":"PPSA01234","title_name":"Stub","used_nullfs":false}"#)
                }
                gen::METHOD_APP_LIST => text_more(
                    r#"{"apps":[{"title_id":"PPSA01234","title_name":"Stub"}]}"#,
                    false,
                ),
                _ => err(gen::ERR_UNKNOWN_METHOD, "unknown method"),
            }
        }),
    )
    .await;
    let out = tokio::task::spawn_blocking(move || {
        let _g = mgmt::scoped_transport(t);
        let r = ps5upload_core::fs_ops::app_register(&c, "/data/games/Stub", false).unwrap();
        let list = ps5upload_core::fs_ops::app_list_registered(&c).unwrap();
        (r.title_id, list.apps.len())
    })
    .await
    .unwrap();
    assert_eq!(out, ("PPSA01234".to_string(), 1));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, gen::METHOD_APP_REGISTER);
    let b: serde_json::Value = serde_json::from_str(&seen[0].1).unwrap();
    assert_eq!(b["src_path"], "/data/games/Stub");
    assert_eq!(
        seen[1].0,
        gen::METHOD_APP_LIST,
        "the readiness gate keeps calling app.list"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_kill_over_ava1_reads_like_the_legacy_one() {
    let (t, c) = console(
        "kill",
        Box::new(|method, _| {
            assert_eq!(method, gen::METHOD_PROC_KILL);
            err(
                gen::ERR_INTERNAL,
                r#"{"ok":false,"pid":99999,"err":"kill_failed","errno":3,"reason":"No such process"}"#,
            )
        }),
    )
    .await;
    let e = tokio::task::spawn_blocking(move || {
        let _g = mgmt::scoped_transport(t);
        ps5upload_core::process_mgr::process_kill(&c, 99999).unwrap_err()
    })
    .await
    .unwrap();
    assert_eq!(
        e.to_string(),
        "PROCESS_KILL failed for pid 99999: No such process"
    );
}
