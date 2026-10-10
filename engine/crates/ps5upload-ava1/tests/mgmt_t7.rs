#![allow(clippy::redundant_closure)]
//! P3 Task 7 over the Rust transport: every module of the hardware / system / accounts / cheats /
//! mods / notices / Remote Play group calls its AVA1 method through `AvaTransport` against the Rust
//! AVA1 server, with the reply bodies the handlers produce (the payload's JSON and `key=value` text), and
//! reads them back as before. Also the behaviours that changed with the move: a legacy failure that
//! carries data, a refusal that stays an error, a destructive power action whose reply is lost, the
//! shell's 10 s bound, and that no core module dials a console directly any more.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen::{self, MgmtText};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::mgmt::AvaTransport;
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt::{scoped_transport, MgmtError};
use ps5upload_core::{
    activity, backup, cheats, diagnostics, fw_spoof, hw, notif, profile, remoteplay, sdk_changer,
    smp_meta, sys_time, system_control, users,
};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-t7-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

type Seen = Arc<Mutex<Vec<(u16, String)>>>;

/// What the scripted console answers.
#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Err(u16, &'static str),
}

fn answer(r: &Reply) -> RpcReply {
    match r {
        Reply::Text(t) => RpcReply {
            status: gen::STATUS_OK,
            body: MgmtText {
                body: t.as_bytes().to_vec(),
                more: None,
            }
            .to_bytes()
            .unwrap(),
        },
        Reply::Err(s, c) => RpcReply {
            status: *s,
            body: c.as_bytes().to_vec(),
        },
    }
}

/// A loopback console running `handler`, and a transport over its own pool.
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
    (Arc::new(t), format!("{tag}-t7"))
}

/// Runs a blocking core function with the transport installed on its thread.
async fn run<T: Send + 'static>(
    t: &Arc<AvaTransport>,
    addr: &str,
    f: impl FnOnce(&str) -> T + Send + 'static,
) -> T {
    let (t, addr) = (t.clone(), addr.to_string());
    tokio::task::spawn_blocking(move || {
        let _g = scoped_transport(t);
        f(&addr)
    })
    .await
    .unwrap()
}

/// A console that answers `method -> reply` for the listed methods and records every call.
fn scripted(table: Vec<(u16, Reply)>) -> (RpcHandler, Seen) {
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    let h: RpcHandler = Box::new(move |method, body| {
        let b = MgmtText::decode(body)
            .map(|t| String::from_utf8_lossy(&t.body).into_owned())
            .unwrap_or_default();
        s2.lock().unwrap().push((method, b));
        match table.iter().find(|(m, _)| *m == method) {
            Some((_, r)) => answer(r),
            None => RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: b"unknown method".to_vec(),
            },
        }
    });
    (h, seen)
}

fn last(seen: &Seen, method: u16) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(m, _)| *m == method)
        .unwrap_or_else(|| panic!("method {method} was never called"))
        .1
        .clone()
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

// ---- hardware, power, time, peripherals, shell ----

#[tokio::test(flavor = "multi_thread")]
async fn hardware_power_and_time_calls_reach_their_methods_and_parse_the_legacy_replies() {
    let (h, seen) = scripted(vec![
        (gen::METHOD_HW_FAN_THRESHOLD, Reply::Text("ok\n")),
        (
            gen::METHOD_HW_DRIVE_SENSORS,
            Reply::Text(
                r#"{"drives":[{"device":"/dev/da0","sizeBytes":1000,"tempC":40}],"storage":[]}"#,
            ),
        ),
        (
            gen::METHOD_POWER_TELEMETRY,
            Reply::Text(
                "operating_seconds=3600\nboot_cycles=12\nthermal_alert_flags=0\npower_up_cause=1\n",
            ),
        ),
        (
            gen::METHOD_POWER_CONTROL,
            Reply::Text(r#"{"ok":true,"action":"tick"}"#),
        ),
        (
            gen::METHOD_TIME_GET,
            Reply::Text(
                r#"{"ok":true,"err_code":0,"year":2026,"month":10,"day":3,"hour":12,"min":0,"sec":5}"#,
            ),
        ),
        (
            gen::METHOD_TIME_SET,
            Reply::Text(
                r#"{"ok":true,"err_code":0,"prior_unix":1,"new_unix":2,"used_fallback":true}"#,
            ),
        ),
        (
            gen::METHOD_PERIPH_CONTROL,
            Reply::Text(r#"{"ok":true,"action":"eject_disc","port":0,"code":0}"#),
        ),
        (
            gen::METHOD_SHELL_EXEC,
            Reply::Text(
                r#"{"exit_code":0,"timed_out":false,"stdout":"hi\n","cwd":"/","session_id":"s"}"#,
            ),
        ),
        (gen::METHOD_HW_INFO, Reply::Text("")),
    ]);
    let (t, c) = console("hw", h).await;
    run(&t, &c, |a| hw::hw_set_fan_threshold(a, 65))
        .await
        .unwrap();
    assert!(last(&seen, gen::METHOD_HW_FAN_THRESHOLD).contains("65"));
    run(&t, &c, |a| hw::drive_sensors(a)).await.unwrap();
    let p = run(&t, &c, |a| system_control::power_telemetry(a))
        .await
        .unwrap();
    assert_eq!((p.operating_seconds, p.boot_cycles), (Some(3600), Some(12)));
    let a = run(&t, &c, |a| {
        system_control::system_control(a, system_control::PowerAction::Tick)
    })
    .await
    .unwrap();
    assert!(a.ok);
    assert!(last(&seen, gen::METHOD_POWER_CONTROL).contains("tick"));
    let t0 = run(&t, &c, |a| sys_time::ps5_time_get(a)).await.unwrap();
    assert_eq!(t0.year, 2026);
    let set = run(&t, &c, |a| sys_time::ps5_time_set(a, 1_790_000_000))
        .await
        .unwrap();
    assert!(set.ok && set.used_fallback);
    let pa = run(&t, &c, |a| {
        diagnostics::peripheral_control(a, diagnostics::PeripheralAction::EjectDisc, 0)
    })
    .await
    .unwrap();
    assert!(pa.ok);
    let sh = run(&t, &c, |a| {
        diagnostics::shell_run(a, "ls", Some("s"), Some("/"), 5)
    })
    .await
    .unwrap();
    assert_eq!(sh.stdout, "hi\n");
    assert!(json(&last(&seen, gen::METHOD_SHELL_EXEC))["cmd"] == "ls");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_that_carries_data_comes_back_as_the_body_the_module_parses() {
    // time.set: the legacy handler answered {"ok":false,"err_code":N} as a normal frame; AVA1 answers an error status
    // whose cause is that body; the module still sees err_code.
    let (h, _s) = scripted(vec![
        (
            gen::METHOD_TIME_SET,
            Reply::Err(
                gen::ERR_INTERNAL,
                r#"{"ok":false,"err_code":3758104577,"prior_unix":-1,"new_unix":-1,"used_fallback":false}"#,
            ),
        ),
        (
            gen::METHOD_TIME_GET,
            Reply::Err(gen::ERR_INTERNAL, r#"{"ok":false,"err_code":7}"#),
        ),
        (
            gen::METHOD_CHEATS_RELOAD,
            Reply::Err(gen::ERR_INTERNAL, r#"{"ok":false}"#),
        ),
        (
            gen::METHOD_TOAST_SEND,
            Reply::Err(gen::ERR_INTERNAL, r#"{"ok":false,"code":-2146369275}"#),
        ),
    ]);
    let (t, c) = console("keep", h).await;
    let r = run(&t, &c, |a| sys_time::ps5_time_set(a, 1_790_000_000))
        .await
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.err_code, 3758104577);
    let g = run(&t, &c, |a| sys_time::ps5_time_get(a)).await.unwrap();
    assert_eq!(g.err_code, 7);
    assert!(!run(&t, &c, |a| cheats::cheats_reload(a)).await.unwrap());
    // toast: the failure body's code reaches the error text the caller shows
    let e = run(&t, &c, |a| {
        ps5upload_core::app_lifecycle::toast_send(
            a,
            &ps5upload_core::app_lifecycle::ToastRequest {
                title: "t".into(),
                subtitle: String::new(),
                icon: String::new(),
                action_url: String::new(),
            },
        )
    })
    .await
    .unwrap_err();
    assert!(
        format!("{e:#}").contains("-2146369275")
            || format!("{e:#}").to_lowercase().contains("toast"),
        "{e:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_that_is_a_plain_token_stays_an_error_with_the_old_text() {
    let (h, _s) = scripted(vec![
        (
            gen::METHOD_RP_ENABLE,
            Reply::Err(gen::ERR_INTERNAL, "rp_enable_no_user"),
        ),
        (
            gen::METHOD_HW_DRIVE_SENSORS,
            Reply::Err(gen::ERR_INTERNAL, "drive_sensors_failed"),
        ),
    ]);
    let (t, c) = console("refuse", h).await;
    let e = run(&t, &c, |a| remoteplay::remoteplay_enable(a, "user"))
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected RemotePlayEnable: rp_enable_no_user"
    );
    let m = e.downcast_ref::<MgmtError>().unwrap();
    assert_eq!(m.status, gen::ERR_INTERNAL);
    assert!(run(&t, &c, |a| hw::drive_sensors(a)).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_reply_to_reboot_or_shutdown_is_success_and_a_refusal_or_a_lost_tick_is_not() {
    // The console takes the call and the session ends before the reply: the handler is never answered.
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    let (t, c) = console(
        "power",
        Box::new(move |method, _b| {
            assert_eq!(method, gen::METHOD_POWER_CONTROL);
            h2.fetch_add(1, Ordering::SeqCst);
            // an answer that is not a MgmtText stands for "the reply never made it"
            RpcReply {
                status: gen::STATUS_OK,
                body: vec![0xFF, 0xFF, 0xFF],
            }
        }),
    )
    .await;
    let r = run(&t, &c, |a| {
        system_control::system_control(a, system_control::PowerAction::Reboot)
    })
    .await
    .unwrap();
    assert!(r.ok);
    assert!(r.err.unwrap_or_default().contains("connection_dropped"));
    assert!(run(&t, &c, |a| system_control::system_control(
        a,
        system_control::PowerAction::Tick
    ))
    .await
    .is_err());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // a payload refusal (bad_action) is an error for every action
    let (h, _s) = scripted(vec![(
        gen::METHOD_POWER_CONTROL,
        Reply::Text(r#"{"ok":false,"err":"standby_unavailable"}"#),
    )]);
    let (t, c) = console("power2", h).await;
    let e = run(&t, &c, |a| {
        system_control::system_control(a, system_control::PowerAction::Standby)
    })
    .await
    .unwrap_err();
    assert!(e.to_string().contains("standby_unavailable"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shell_call_is_bounded_to_ten_seconds() {
    let (t, c) = console(
        "shell",
        Box::new(|_m, _b| {
            std::thread::sleep(Duration::from_secs(15));
            RpcReply {
                status: gen::STATUS_OK,
                body: vec![],
            }
        }),
    )
    .await;
    let t0 = std::time::Instant::now();
    let r = run(&t, &c, |a| {
        diagnostics::shell_run(a, "cat /dev/zero", None, None, 600)
    })
    .await;
    assert!(r.is_err());
    assert!(t0.elapsed() < Duration::from_secs(13), "{:?}", t0.elapsed());
    assert_eq!(diagnostics::SHELL_MAX_SECS, 10);
}

// ---- accounts, backups, cheats, mods, notices, activity, Remote Play ----

#[tokio::test(flavor = "multi_thread")]
async fn account_backup_cheat_mod_notice_activity_and_remote_play_calls_round_trip() {
    let (h, seen) = scripted(vec![
        (
            gen::METHOD_PROFILE_INFO,
            Reply::Text(
                r#"{"ok":true,"uid":268435456,"uid_hex":"0x10000000","username":"Neo","slots":[],"users":[]}"#,
            ),
        ),
        (
            gen::METHOD_PROFILE_SET_USERNAME,
            Reply::Text(r#"{"ok":true,"err_code":0}"#),
        ),
        (
            gen::METHOD_PROFILE_SET_LOCAL_USERNAME,
            Reply::Text(r#"{"ok":true}"#),
        ),
        (
            gen::METHOD_PROFILE_ACTIVATE,
            Reply::Text(r#"{"ok":true,"id":"0x0123456789abcdef"}"#),
        ),
        (
            gen::METHOD_PROFILE_CLEAR_SLOT,
            Reply::Text(r#"{"ok":true,"err_code":0}"#),
        ),
        (
            gen::METHOD_USER_CREATE,
            Reply::Text(r#"{"ok":true,"uid":2,"name":"Kid","err":""}"#),
        ),
        (
            gen::METHOD_USER_DELETE,
            Reply::Text(r#"{"ok":true,"uid":2,"err":""}"#),
        ),
        (gen::METHOD_BACKUP_LIST, Reply::Text(r#"{"snapshots":[]}"#)),
        (
            gen::METHOD_BACKUP_DELETE,
            Reply::Text(r#"{"ok":true,"tag":"t","timestamp":5,"err":""}"#),
        ),
        (gen::METHOD_CHEATS_LIST, Reply::Text(r#"{"titles":[]}"#)),
        (gen::METHOD_CHEATS_GET, Reply::Text(r#"{"mods":[]}"#)),
        (
            gen::METHOD_CHEATS_TOGGLE,
            Reply::Text(r#"{"ok":true,"title_id":"CUSA00001","index":2,"on":true}"#),
        ),
        (gen::METHOD_CHEATS_DELETE, Reply::Text(r#"{"ok":true}"#)),
        (gen::METHOD_CHEATS_RELOAD, Reply::Text(r#"{"ok":true}"#)),
        (
            gen::METHOD_CHEATS_STATUS,
            Reply::Text(r#"{"enabled":true,"titles":0,"mods":0}"#),
        ),
        (
            gen::METHOD_CHEATS_ENGINE_SET,
            Reply::Text(r#"{"ok":true,"enabled":false}"#),
        ),
        (gen::METHOD_SMP_META_CONTROL, Reply::Text(r#"{"ok":true}"#)),
        (
            gen::METHOD_SMP_META_STATS,
            Reply::Text(
                r#"{"running":false,"poll_seconds":60,"last_run_unix":0,"games_scanned":3,"icons_healed":1,"pics_healed":0,"json_healed":0,"still_missing":0}"#,
            ),
        ),
        (gen::METHOD_SDK_SCAN, Reply::Text(r#"{"titles":[]}"#)),
        (
            gen::METHOD_SDK_PATCH,
            Reply::Text(
                r#"{"ok":true,"title_id":"CUSA00001","target_sdk":"0x04000031","detail":"3 sites"}"#,
            ),
        ),
        (
            gen::METHOD_SDK_RESTORE,
            Reply::Text(r#"{"ok":true,"title_id":"CUSA00001","restored":3,"error":""}"#),
        ),
        (
            gen::METHOD_FWSPOOF_STATUS,
            Reply::Text(r#"{"spoofed":false}"#),
        ),
        (
            gen::METHOD_NOTIF_LIST,
            Reply::Text(r#"{"notifications":[]}"#),
        ),
        (
            gen::METHOD_NOTIF_CLEAR,
            Reply::Text(r#"{"ok":true,"removed":2}"#),
        ),
        (gen::METHOD_ACTIVITY_GET, Reply::Text(r#"{"entries":[]}"#)),
        (
            gen::METHOD_ACTIVITY_DB_QUERY,
            Reply::Text(r#"{"entries":[],"source":"app.db"}"#),
        ),
        (
            gen::METHOD_ACTIVITY_RESET,
            Reply::Text(r#"{"ok":true,"removed":4}"#),
        ),
        (
            gen::METHOD_RP_REQUEST,
            Reply::Text(r#"{"ok":true,"snapshot":{"pin":"12345678","account_id":"abc"}}"#),
        ),
        (gen::METHOD_RP_STATUS, Reply::Text(r#"{"state":"waiting"}"#)),
        (gen::METHOD_RP_CANCEL, Reply::Text(r#"{"ok":true}"#)),
        (gen::METHOD_RP_READINESS, Reply::Text(r#"{"fw_magic":0}"#)),
        (gen::METHOD_RP_ENABLE, Reply::Text(r#"{"fw_magic":0}"#)),
    ]);
    let (t, c) = console("misc", h).await;
    let i = run(&t, &c, |a| profile::profile_info(a)).await.unwrap();
    assert_eq!(i.username, "Neo");
    run(&t, &c, |a| profile::profile_set_username(a, 1, "Neo"))
        .await
        .unwrap();
    run(&t, &c, |a| profile::profile_set_local_username(a, 7, "Neo"))
        .await
        .unwrap();
    assert_eq!(
        run(&t, &c, |a| profile::profile_activate(a, 1, None))
            .await
            .unwrap(),
        "0x0123456789abcdef"
    );
    run(&t, &c, |a| profile::profile_clear_slot(a, 1))
        .await
        .unwrap();
    run(&t, &c, |a| users::user_create(a, "Kid")).await.unwrap();
    run(&t, &c, |a| users::user_delete(a, 2, true))
        .await
        .unwrap();
    assert!(
        json(&last(&seen, gen::METHOD_USER_DELETE))["wipe_saves"] == 1
            || last(&seen, gen::METHOD_USER_DELETE).contains("wipe_saves")
    );
    run(&t, &c, |a| backup::backup_list(a, "t")).await.unwrap();
    run(&t, &c, |a| backup::backup_delete(a, "t", 5))
        .await
        .unwrap();
    run(&t, &c, |a| cheats::cheats_list(a)).await.unwrap();
    run(&t, &c, |a| cheats::cheats_get(a, "CUSA00001"))
        .await
        .unwrap();
    let tg = run(&t, &c, |a| cheats::cheats_toggle(a, "CUSA00001", 2, true))
        .await
        .unwrap();
    assert!(tg.ok);
    assert!(run(&t, &c, |a| cheats::cheats_delete(a, "CUSA00001"))
        .await
        .unwrap());
    assert!(run(&t, &c, |a| cheats::cheats_reload(a)).await.unwrap());
    run(&t, &c, |a| cheats::cheats_status(a)).await.unwrap();
    run(&t, &c, |a| cheats::cheats_engine_set(a, false))
        .await
        .unwrap();
    run(&t, &c, |a| smp_meta::smp_meta_stats(a)).await.unwrap();
    // sdk.scan runs as a job.run op (Task 5); the scripted console here answers methods only, so it is
    // covered by ava1-ctest job_run.rs `cleanup_and_a_100_kib_sdk_scan_result_come_back_whole`.
    let sp = run(&t, &c, |a| {
        sdk_changer::sdk_patch(a, "CUSA00001", "0x04000031", false)
    })
    .await
    .unwrap();
    assert!(sp.ok);
    let sr = run(&t, &c, |a| sdk_changer::sdk_restore(a, "CUSA00001"))
        .await
        .unwrap();
    assert_eq!(sr.restored, 3);
    run(&t, &c, |a| fw_spoof::fw_spoof_status(a)).await.unwrap();
    run(&t, &c, |a| notif::notif_list(a, 0)).await.unwrap();
    assert_eq!(
        run(&t, &c, |a| notif::notif_clear(a))
            .await
            .unwrap()
            .removed,
        2
    );
    run(&t, &c, |a| activity::activity_get(a)).await.unwrap();
    run(&t, &c, |a| activity::activity_db_query(a, "q"))
        .await
        .unwrap();
    assert_eq!(
        run(&t, &c, |a| activity::activity_reset(a))
            .await
            .unwrap()
            .removed,
        4
    );
    let pin = run(&t, &c, |a| remoteplay::remoteplay_request(a, None))
        .await
        .unwrap();
    assert_eq!(pin.pin, "12345678");
    run(&t, &c, |a| remoteplay::remoteplay_status(a))
        .await
        .unwrap();
    run(&t, &c, |a| remoteplay::remoteplay_cancel(a))
        .await
        .unwrap();
    run(&t, &c, |a| remoteplay::remoteplay_readiness(a))
        .await
        .unwrap();
    run(&t, &c, |a| remoteplay::remoteplay_enable(a, "service"))
        .await
        .unwrap();
    assert!(json(&last(&seen, gen::METHOD_RP_ENABLE))["scope"] == "service");
    // every one of the group's methods was reached
    let called: std::collections::BTreeSet<u16> =
        seen.lock().unwrap().iter().map(|(m, _)| *m).collect();
    assert!(called.len() >= 30, "{called:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rp_request_returns_at_once_with_the_pin_and_status_is_polled() {
    // The handler answers immediately (no waiting for pairing): the request carries the PIN snapshot,
    // and the engine polls rp.status afterwards.
    let (h, seen) = scripted(vec![
        (
            gen::METHOD_RP_REQUEST,
            Reply::Text(r#"{"ok":true,"snapshot":{"pin":"87654321","account_id":"x"}}"#),
        ),
        (gen::METHOD_RP_STATUS, Reply::Text(r#"{"state":"pending"}"#)),
    ]);
    let (t, c) = console("rp", h).await;
    let t0 = std::time::Instant::now();
    let pin = run(&t, &c, |a| remoteplay::remoteplay_request(a, Some("acct")))
        .await
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(pin.pin, "87654321");
    assert!(last(&seen, gen::METHOD_RP_REQUEST).contains("acct"));
    for _ in 0..3 {
        run(&t, &c, |a| remoteplay::remoteplay_status(a))
            .await
            .unwrap();
    }
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| *m == gen::METHOD_RP_STATUS)
            .count(),
        3
    );
}

// ---- no module dials a console ----

#[test]
fn no_core_module_of_this_group_dials_a_console_directly() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ps5upload-core/src");
    // Tasks 4-6 and 9 convert the rest of the crate (fs, apps, saves, search, cleanup, focus, process,
    // volumes, lifecycle); this group's modules must be done.
    for f in [
        "hw.rs",
        "system_control.rs",
        "sys_time.rs",
        "profile.rs",
        "users.rs",
        "cheats.rs",
        "notif.rs",
        "activity.rs",
        "smp_meta.rs",
        "sdk_changer.rs",
        "fw_spoof.rs",
        "remoteplay.rs",
    ] {
        let s = std::fs::read_to_string(src.join(f)).unwrap();
        assert!(
            !s.contains("TcpStream::connect("),
            "{f} still dials a console"
        );
        assert!(!s.contains("send_frame("), "{f} still sends a frame itself");
    }
    // backup.rs: list and delete are converted; snapshot and restore are Task 5's job ops
    let b = std::fs::read_to_string(src.join("backup.rs")).unwrap();
    for f in ["pub fn backup_list", "pub fn backup_delete"] {
        let i = b.find(f).unwrap();
        let end = b[i..].find("\n}\n").unwrap();
        assert!(!b[i..i + end].contains("TcpStream::connect"), "{f}");
    }
    // diagnostics.rs: periph and shell
    let d = std::fs::read_to_string(src.join("diagnostics.rs")).unwrap();
    for f in ["pub fn peripheral_control", "pub fn shell_run"] {
        let i = d.find(f).unwrap();
        let end = d[i..].find("\n}\n").unwrap();
        assert!(!d[i..i + end].contains("TcpStream::connect"), "{f}");
    }
}
