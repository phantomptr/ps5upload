//! P3 Task 6: the apps / launch / process / media / search-index call sites against a fake
//! transport. Each keeps its public signature, its legacy request and reply bodies and its
//! error text; the seam carries the AVA1 method, the label and the deadline.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ps5upload_core::mgmt::{self, m, Method, MgmtError, MgmtTransport};

type Seen = Vec<(u16, String, Vec<u8>, Duration)>;
type Script = Box<dyn Fn(Method, &[u8]) -> Result<Option<Vec<u8>>> + Send + Sync>;

struct Fake {
    seen: Mutex<Seen>,
    reply: Script,
}

impl MgmtTransport for Fake {
    fn call(
        &self,
        _addr: &str,
        method: Method,
        label: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>> {
        self.seen
            .lock()
            .unwrap()
            .push((method.id, label.into(), body.to_vec(), timeout));
        (self.reply)(method, body)
    }
}

fn fake(
    f: impl Fn(Method, &[u8]) -> Result<Option<Vec<u8>>> + Send + Sync + 'static,
) -> (Arc<Fake>, mgmt::ScopedTransport) {
    let t = Arc::new(Fake {
        seen: Mutex::default(),
        reply: Box::new(f),
    });
    let g = mgmt::scoped_transport(t.clone());
    (t, g)
}

fn reply(b: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(Some(b.to_vec()))
}

fn refuse(label: &str, status: u16, cause: &str) -> Result<Option<Vec<u8>>> {
    Err(MgmtError {
        label: label.into(),
        status,
        cause: cause.into(),
    }
    .into())
}

const A: &str = "10.0.0.5";

#[test]
fn register_unregister_and_launch_send_their_bodies_with_a_sixty_second_deadline() {
    let (t, _g) = fake(|method, _| match method.id {
        48 => reply(br#"{"title_id":"PPSA01234","title_name":"Game","used_nullfs":true}"#),
        49 => reply(br#"{"sony_uninstall_rc":2158631682}"#),
        _ => reply(b""),
    });
    let r = ps5upload_core::fs_ops::app_register(A, "/data/games/X", true).unwrap();
    assert_eq!(r.title_id, "PPSA01234");
    let u = ps5upload_core::fs_ops::app_unregister(A, "PPSA01234").unwrap();
    assert!(u.sony_refused());
    assert_eq!(u.sony_uninstall_rc, 2158631682);
    ps5upload_core::fs_ops::app_launch(A, "PPSA01234").unwrap();
    let seen = t.seen.lock().unwrap();
    assert_eq!(
        (seen[0].0, seen[0].1.as_str(), seen[0].3),
        (48, "APP_REGISTER(/data/games/X)", Duration::from_secs(60))
    );
    let b: serde_json::Value = serde_json::from_slice(&seen[0].2).unwrap();
    assert_eq!(
        (b["src_path"].clone(), b["patch_drm_type"].clone()),
        ("/data/games/X".into(), 1.into())
    );
    assert_eq!(
        (seen[1].0, seen[1].1.as_str(), seen[1].3),
        (49, "APP_UNREGISTER", Duration::from_secs(60))
    );
    assert_eq!(
        (seen[2].0, seen[2].1.as_str(), seen[2].3),
        (50, "APP_LAUNCH", Duration::from_secs(60))
    );
    assert_eq!(seen[2].2, br#"{"title_id":"PPSA01234"}"#);
}

#[test]
fn an_empty_unregister_reply_still_means_sony_rc_zero() {
    let (_t, _g) = fake(|_, _| reply(b""));
    let u = ps5upload_core::fs_ops::app_unregister(A, "X").unwrap();
    assert_eq!(u.sony_uninstall_rc, 0);
}

#[test]
fn a_refused_register_keeps_the_text_callers_and_the_ui_match_on() {
    let (_t, _g) = fake(|_, _| {
        refuse(
            "APP_REGISTER(/forbidden)",
            9,
            "register_src_path_not_allowed",
        )
    });
    let e = ps5upload_core::fs_ops::app_register(A, "/forbidden", false).unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected APP_REGISTER(/forbidden): register_src_path_not_allowed"
    );
}

#[test]
fn a_failed_launch_is_the_payload_rejected_text() {
    let (_t, _g) = fake(|_, _| {
        refuse(
            "APP_LAUNCH",
            7,
            "launch_all_strategies_failed: param=1 null=1 sys=1",
        )
    });
    let e = ps5upload_core::fs_ops::app_launch(A, "PPSA01234").unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected APP_LAUNCH: launch_all_strategies_failed: param=1 null=1 sys=1"
    );
}

#[test]
fn app_list_decodes_the_merged_document() {
    let (t, _g) = fake(|_, _| {
        reply(br#"{"apps":[{"title_id":"PPSA1","title_name":"A"},{"title_id":"PPSA2","title_name":"B","src":"/data/x","image_backed":true}]}"#)
    });
    let l = ps5upload_core::fs_ops::app_list_registered(A).unwrap();
    assert_eq!(l.apps.len(), 2);
    assert!(l.apps[1].image_backed);
    let seen = t.seen.lock().unwrap();
    assert_eq!((seen[0].0, seen[0].1.as_str()), (51, "APP_LIST_REGISTERED"));
}

#[test]
fn a_kill_the_console_refused_keeps_its_errno_text_in_the_error() {
    let body =
        r#"{"ok":false,"pid":99999,"err":"kill_failed","errno":3,"reason":"No such process"}"#;
    let (t, _g) = fake(move |_, _| refuse("PROCESS_KILL", 7, body));
    let e = ps5upload_core::process_mgr::process_kill(A, 99999).unwrap_err();
    assert_eq!(
        e.to_string(),
        "PROCESS_KILL failed for pid 99999: No such process",
        "the strerror text the user sees must survive the transport change"
    );
    assert_eq!(t.seen.lock().unwrap()[0].0, 60);
    // and a kill that worked
    let (_t, _g2) = fake(|_, _| reply(br#"{"ok":true,"pid":42}"#));
    assert_eq!(
        ps5upload_core::process_mgr::process_kill(A, 42)
            .unwrap()
            .pid,
        42
    );
}

#[test]
fn a_lifecycle_refusal_still_reports_the_sony_return_code() {
    let body = r#"{"ok":false,"action":"kill","app_id":666,"code":-2146369530}"#;
    let (_t, _g) = fake(move |_, _| refuse("APP_LIFECYCLE", 7, body));
    let e = ps5upload_core::app_lifecycle::app_lifecycle(
        A,
        ps5upload_core::app_lifecycle::AppAction::Kill,
        666,
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "APP_LIFECYCLE failed: console returned {:#010x}",
            -2146369530i32
        )
    );
    // a bad action carries its err token
    let (_t, _g2) = fake(|_, _| refuse("APP_LIFECYCLE", 4, r#"{"ok":false,"err":"bad_action"}"#));
    let e = ps5upload_core::app_lifecycle::app_lifecycle(
        A,
        ps5upload_core::app_lifecycle::AppAction::Suspend,
        1,
    )
    .unwrap_err();
    assert_eq!(e.to_string(), "APP_LIFECYCLE failed: bad_action");
    // the list action
    let (_t, _g3) = fake(|_, _| reply(br#"{"ok":true,"action":"list","apps":[{"app_id":57368}]}"#));
    let a = ps5upload_core::app_lifecycle::app_lifecycle(
        A,
        ps5upload_core::app_lifecycle::AppAction::List,
        0,
    )
    .unwrap();
    assert_eq!(a.apps[0].app_id, 57368);
}

#[test]
fn appinfo_failures_decode_as_results_not_as_errors() {
    let (_t, _g) = fake(|_, _| {
        refuse(
            "APPINFO_QUERY",
            4,
            r#"{"ok":false,"error":"title_id is required"}"#,
        )
    });
    let r = ps5upload_core::diagnostics::appinfo_query(A, "", None).unwrap();
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("title_id is required"));
    let (_t, _g2) =
        fake(|_, _| refuse("APPINFO_SET", 7, r#"{"ok":false,"err":"title is running"}"#));
    let s = ps5upload_core::diagnostics::appinfo_set(A, "X", "k", "v").unwrap();
    assert!(!s.ok);
    assert_eq!(s.err.as_deref(), Some("title is running"));
    // an error that is only a token (an ERROR frame) is still an error
    let (_t, _g3) = fake(|_, _| refuse("APPINFO_SET", 4, "appinfo_oom"));
    assert!(ps5upload_core::diagnostics::appinfo_set(A, "X", "k", "v").is_err());
}

#[test]
fn processes_focus_database_and_modules_read_their_replies() {
    let (t, _g) = fake(|method, _| {
        match method.id {
        59 => reply(br#"{"procs":[{"pid":101,"name":"payload.elf","kind":"payload","is_self":true},{"pid":0,"truncated":true}]}"#),
        57 => reply(br#"{"ok":true,"apis":{"sceX":true},"big_app_id":57368}"#),
        56 => reply(br#"{"apps":[{"title_id":"CUSA1","app_id":1,"name":"N"}],"source":"sqlite"}"#),
        61 => reply(br#"{"modules":[]}"#),
        _ => reply(b"{}"),
    }
    });
    let p = ps5upload_core::process_mgr::process_list(A).unwrap();
    assert!(p.truncated && p.processes.len() == 1 && p.processes[0].is_self);
    let f = ps5upload_core::focus::focus_probe(A).unwrap();
    assert_eq!(f.big_app_id, 57368);
    let d = ps5upload_core::diagnostics::appdb_query(A).unwrap();
    assert_eq!(d.apps.len(), 1);
    ps5upload_core::diagnostics::proc_modules(A, 101).unwrap();
    let seen = t.seen.lock().unwrap();
    assert_eq!(
        seen.iter().map(|s| (s.0, s.1.as_str())).collect::<Vec<_>>(),
        vec![
            (59, "PROCESS_LIST"),
            (57, "FOCUS_PROBE"),
            (56, "APP_DB_QUERY"),
            (61, "PROC_MODULES")
        ]
    );
    assert_eq!(seen[3].2, br#"{"pid":101}"#);
}

#[test]
fn saves_screenshots_and_videos_decode_and_surface_truncation() {
    let (t, _g) = fake(|method, _| {
        match method.id {
        64 => reply(br#"{"saves":[{"title_id":"CUSA1","user_id":1,"path":"/p","size":1,"mtime":2,"kind":"ps4"}],"truncated":true}"#),
        65 => reply(br#"{"items":[{"path":"/a.jxr","size":1,"mtime":2}]}"#),
        _ => reply(br#"{"items":[]}"#),
    }
    });
    let s = ps5upload_core::saves::list_saves(A, 7).unwrap();
    assert!(s.truncated && s.saves.len() == 1);
    let sh = ps5upload_core::saves::list_screenshots(A).unwrap();
    assert!(!sh.truncated && sh.items.len() == 1);
    assert!(ps5upload_core::saves::list_videos(A)
        .unwrap()
        .items
        .is_empty());
    let seen = t.seen.lock().unwrap();
    assert_eq!(seen[0].2, br#"{"user_id":7}"#);
    assert_eq!(
        seen.iter().map(|s| s.0).collect::<Vec<_>>(),
        vec![64, 65, 66]
    );
}

#[test]
fn the_search_index_methods_keep_their_bodies_and_the_truncated_mark() {
    let (t, _g) = fake(|method, _| match method.id {
        67 => reply(br#"{"started":false,"err":"already_building"}"#),
        68 => reply(
            br#"{"phase":"building","files":12,"truncated":false,"started_at":1,"completed_at":0}"#,
        ),
        69 => reply(br#"{"results":[{"path":"/a.pkg","size":5}],"truncated":true}"#),
        _ => reply(br#"{"cancelled":true}"#),
    });
    let st = ps5upload_core::search_index::index_start(A, &["/data", "/user"]).unwrap();
    assert!(!st.started && st.err.as_deref() == Some("already_building"));
    assert_eq!(
        ps5upload_core::search_index::index_status(A).unwrap().files,
        12
    );
    let r = ps5upload_core::search_index::search_index(
        A,
        &ps5upload_core::search_index::SearchQuery {
            query: "*.pkg".into(),
            size_min: 0,
            size_max: 0,
            limit: 5,
        },
    )
    .unwrap();
    assert!(r.truncated && r.results.len() == 1);
    ps5upload_core::search_index::index_cancel(A).unwrap();
    let seen = t.seen.lock().unwrap();
    assert_eq!(seen[0].2, br#"{"roots":["/data","/user"]}"#);
    assert_eq!(
        seen.iter().map(|s| s.0).collect::<Vec<_>>(),
        vec![67, 68, 69, 70]
    );
    // an older payload's reply has no `truncated`: it reads false
    let (_t, _g2) = fake(|_, _| reply(br#"{"results":[]}"#));
    let r = ps5upload_core::search_index::search_index(
        A,
        &ps5upload_core::search_index::SearchQuery {
            query: "*".into(),
            size_min: 0,
            size_max: 0,
            limit: 0,
        },
    )
    .unwrap();
    assert!(!r.truncated);
}

#[test]
fn the_browser_and_process_list_methods_are_the_ones_the_table_names() {
    let (t, _g) = fake(|_, _| reply(b"{}"));
    ps5upload_core::hw::app_launch_browser(A).unwrap();
    assert_eq!(t.seen.lock().unwrap()[0].0, m::APP_LAUNCH_BROWSER.id);
}
