//! AVA1 integration tests for the hardware, app-lifecycle, process, shell and volume management
//! methods. They replace `hw_integration.rs` and `volumes_integration.rs`: the same fixtures the
//! old in-process mock server returned are now answered by a scripted AVA1 management node, so a
//! regression in the key=value and JSON body parsing in `ps5upload-core`, in the AVA1 transport's
//! text/JSON conversion, or in the concurrent-request behaviour of the engine's management
//! gate surfaces here. No console is involved.

mod ava1_common;
use ava1_common::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ava1::gen;
use ava1::server::RpcHandler;
use ava1::session::RpcReply;
use ps5upload_core::diagnostics::shell_run;
use ps5upload_core::fs_ops::{app_launch, app_list_registered, app_register};
use ps5upload_core::hw::{
    app_launch_browser, hw_info, hw_power, hw_set_fan_threshold, hw_temps, proc_list,
};
use ps5upload_core::volumes::list_volumes;

const DEFAULT_VOLUMES_JSON: &str = r#"{"volumes":[
{"path":"/data","fs_type":"ufs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true},
{"path":"/ext0","fs_type":"ufs","total_bytes":1000000000000,"free_bytes":900000000000,"writable":true}
]}"#;

/// What the scripted console keeps between calls.
#[derive(Default)]
struct State {
    volumes_json: Mutex<Option<String>>,
    shell_sessions: Mutex<HashMap<String, String>>,
}

fn json_str(body: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

/// The retired mock server's fixtures, as AVA1 management replies.
fn script(state: Arc<State>) -> RpcHandler {
    Box::new(move |method, req| {
        let body = text_of(req);
        match method {
            gen::METHOD_FS_VOLUMES => text(
                state
                    .volumes_json
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or(DEFAULT_VOLUMES_JSON),
            ),
            gen::METHOD_APP_REGISTER => {
                let src = json_str(&body, "src_path").unwrap_or_default();
                if src.is_empty() {
                    return refuse(gen::ERR_PATH, "register_src_path_missing");
                }
                // "PPSA" + up to five alphanumeric characters of the basename, as the mock did.
                let leaf = src.rsplit('/').next().unwrap_or("UNKNOWN");
                let tail: String = leaf
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .take(5)
                    .collect();
                text(&format!(
                    "{{\"title_id\":\"PPSA{tail:0>5}\",\"title_name\":\"{leaf}\",\"used_nullfs\":true}}"
                ))
            }
            gen::METHOD_APP_LAUNCH => {
                if json_str(&body, "title_id").unwrap_or_default().is_empty() {
                    return refuse(gen::ERR_PATH, "launch_title_id_missing");
                }
                text("")
            }
            gen::METHOD_APP_LIST => text(
                "{\"apps\":[\
                 {\"title_id\":\"PPSA00123\",\"title_name\":\"Test Game\",\
                 \"src\":\"/data/homebrew/test\",\"image_backed\":false},\
                 {\"title_id\":\"CUSA00456\",\"title_name\":\"Backported\",\
                 \"src\":\"/mnt/ps5upload/bp/game\",\"image_backed\":true}]}",
            ),
            gen::METHOD_HW_INFO => text(
                "model=CFI-1215A\nserial=TEST-SERIAL-123\nhas_wlan_bt=1\nhas_optical_out=0\n\
                 hw_model=CFI-1215A\nhw_machine=amd64\nos=FreeBSD 11.0\nncpu=8\n\
                 physmem=13958643712\n",
            ),
            gen::METHOD_HW_TEMPS => {
                // Any non-empty body selects the EXTENDED read (the engine sends "ufs").
                if body.is_empty() {
                    text(
                        "cpu_temp=65\nsoc_temp=72\ncpu_freq_mhz=3500\nsoc_clock_mhz=0\n\
                         soc_power_mw=85000\n",
                    )
                } else {
                    text(
                        "cpu_temp=65\nsoc_temp=72\ncpu_freq_mhz=3500\nsoc_clock_mhz=0\n\
                         soc_power_mw=85000\ncpu_usage_pct=37\nfan_duty_pct=55\nproduct_shape=2\n",
                    )
                }
            }
            gen::METHOD_HW_POWER => text(
                "operating_time_sec=7230\noperating_time_hours=2\noperating_time_minutes=0\n\
                 boot_count=0\npower_consumption_mw=0\n",
            ),
            gen::METHOD_APP_LAUNCH_BROWSER | gen::METHOD_HW_FAN_THRESHOLD => text(""),
            gen::METHOD_PROC_LIST => text(
                r#"{"ok":true,"procs":[{"pid":97,"name":"SceShellUI"},{"pid":113,"name":"payload.elf"}]}"#,
            ),
            gen::METHOD_SHELL_EXEC => shell(&state, &body),
            _ => refuse(gen::ERR_UNKNOWN_METHOD, "unsupported_method"),
        }
    })
}

/// A tiny shell that keeps a working directory per session id (`pwd`, `ls`, `cd`).
fn shell(state: &State, body: &str) -> RpcReply {
    let cmd = json_str(body, "cmd").unwrap_or_default();
    let session = json_str(body, "session_id").unwrap_or_else(|| "default".to_string());
    let fallback = json_str(body, "cwd")
        .filter(|v| v.starts_with('/'))
        .unwrap_or_else(|| "/".to_string());
    let mut sessions = state.shell_sessions.lock().unwrap();
    let cwd = sessions.entry(session.clone()).or_insert(fallback);
    let (mut exit_code, mut stdout) = (0, String::new());
    if cmd == "pwd" {
        stdout = format!("{cwd}\n");
    } else if cmd == "ls" {
        stdout = format!("listed:{cwd}\n");
    } else if let Some(target) = cmd.strip_prefix("cd ") {
        if target.starts_with('/') {
            *cwd = target.to_string();
        } else if cwd == "/" {
            *cwd = format!("/{target}");
        } else {
            *cwd = format!("{cwd}/{target}");
        }
    } else {
        exit_code = 127;
        stdout = format!("{cmd}: command not found\n");
    }
    text(
        &serde_json::json!({
            "exit_code": exit_code,
            "timed_out": false,
            "stdout": stdout,
            "cwd": *cwd,
            "session_id": session,
        })
        .to_string(),
    )
}

fn node() -> (Node, Arc<State>) {
    let state = Arc::new(State::default());
    (Node::start(script(state.clone())), state)
}

// ─── HW_INFO: static fixtures round-trip cleanly ───────────────────────────────

/// Ports `hw_info_round_trip`.
#[test]
fn hw_info_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let info = hw_info(CONSOLE).expect("hw_info");
    assert_eq!(info.model, "CFI-1215A");
    assert_eq!(info.serial, "TEST-SERIAL-123");
    assert!(info.has_wlan_bt);
    assert!(!info.has_optical_out);
    assert_eq!(info.os, "FreeBSD 11.0");
    assert_eq!(info.ncpu, 8);
    assert_eq!(info.physmem, 13_958_643_712);
}

/// Ports `hw_temps_round_trip`: the BASIC read omits the on-demand telemetry (-1).
#[test]
fn hw_temps_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let t = hw_temps(CONSOLE, false).expect("hw_temps");
    assert_eq!(t.cpu_temp, 65);
    assert_eq!(t.soc_temp, 72);
    assert_eq!(t.cpu_freq_mhz, 3500);
    assert_eq!(t.soc_power_mw, 85_000);
    assert_eq!(t.cpu_usage_pct, -1, "basic read omits CPU usage");
    assert_eq!(t.fan_duty_pct, -1, "basic read omits fan duty");
    assert_eq!(t.product_shape, -1, "basic read omits product shape");
}

/// Ports `hw_temps_extended_round_trip`.
#[test]
fn hw_temps_extended_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let t = hw_temps(CONSOLE, true).expect("hw_temps extended");
    assert_eq!(t.cpu_temp, 65);
    assert_eq!(t.soc_power_mw, 85_000);
    assert_eq!(t.cpu_usage_pct, 37);
    assert_eq!(t.fan_duty_pct, 55);
    assert_eq!(t.product_shape, 2);
}

/// Ports `hw_power_round_trip`.
#[test]
fn hw_power_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let p = hw_power(CONSOLE).expect("hw_power");
    assert_eq!(p.operating_time_sec, 7230);
    assert_eq!(p.operating_time_hours, 2);
    assert_eq!(p.operating_time_minutes, 0);
}

/// Ports `launch_browser_completes`.
#[test]
fn launch_browser_completes() {
    let (n, _) = node();
    let _g = n.attach();
    app_launch_browser(CONSOLE).expect("launch_browser");
}

/// Ports `concurrent_hw_requests_dont_race`: twenty clients mix request types at once, well above
/// the ~8 a Library refresh produces, sharing one session through the engine's management gate.
/// Every call must answer (none lost, none panicking).
#[test]
fn concurrent_hw_requests_dont_race() {
    let (n, _) = node();
    let t = n.transport.clone();
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let t = t.clone();
            std::thread::spawn(move || {
                let _g = ps5upload_core::mgmt::scoped_transport(t);
                hw_info(CONSOLE).expect("hw_info");
                hw_temps(CONSOLE, false).expect("hw_temps");
                hw_power(CONSOLE).expect("hw_power");
                app_list_registered(CONSOLE).expect("app_list_registered");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread should not panic");
    }
}

// ─── Register + launch round-trip ──────────────────────────────────────────────

/// Ports `register_then_launch_round_trip`.
#[test]
fn register_then_launch_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let reg =
        app_register(CONSOLE, "/data/homebrew/test-game", false).expect("register should succeed");
    assert!(reg.title_id.starts_with("PPSA"));
    assert!(reg.used_nullfs);
    app_launch(CONSOLE, &reg.title_id).expect("launch should succeed");
}

/// Ports `register_rejects_empty_path`: the console's own reason reaches the caller.
#[test]
fn register_rejects_empty_path() {
    let (n, _) = node();
    let _g = n.attach();
    let err = app_register(CONSOLE, "", false).expect_err("empty path must fail");
    assert!(
        err.to_string().contains("register_src_path_missing"),
        "error should surface payload-side reason: {err}"
    );
}

/// Ports `launch_rejects_empty_title_id`.
#[test]
fn launch_rejects_empty_title_id() {
    let (n, _) = node();
    let _g = n.attach();
    let err = app_launch(CONSOLE, "").expect_err("empty title_id must fail");
    assert!(
        err.to_string().contains("launch_title_id_missing"),
        "error should surface payload-side reason: {err}"
    );
}

// ─── Fan threshold: request-path plumbing + clamp enforcement ──────────────────

/// Ports `fan_threshold_round_trip_in_safe_range`.
#[test]
fn fan_threshold_round_trip_in_safe_range() {
    let (n, _) = node();
    let _g = n.attach();
    hw_set_fan_threshold(CONSOLE, 65).expect("fan threshold should succeed");
    hw_set_fan_threshold(CONSOLE, 45).expect("floor should be allowed");
    hw_set_fan_threshold(CONSOLE, 80).expect("ceiling should be allowed");
}

/// Ports `fan_threshold_rejects_below_floor`.
#[test]
fn fan_threshold_rejects_below_floor() {
    let (n, _) = node();
    let _g = n.attach();
    let err = hw_set_fan_threshold(CONSOLE, 30).expect_err("below-floor threshold is rejected");
    assert!(
        err.to_string().contains("safe range"),
        "error should surface range info: {err}"
    );
}

/// Ports `fan_threshold_rejects_above_ceiling`.
#[test]
fn fan_threshold_rejects_above_ceiling() {
    let (n, _) = node();
    let _g = n.attach();
    let err = hw_set_fan_threshold(CONSOLE, 95).expect_err("above-ceiling threshold is rejected");
    assert!(err.to_string().contains("safe range"));
}

// ─── PROC_LIST ─────────────────────────────────────────────────────────────────

/// Ports `proc_list_round_trip`: the sysctl-shaped body parses, and SceShellUI (the pid the
/// ShellUI RPC layer keys on) is there.
#[test]
fn proc_list_round_trip() {
    let (n, _) = node();
    let _g = n.attach();
    let list = proc_list(CONSOLE).expect("proc_list");
    assert!(list.ok, "ok flag should propagate from sysctl shape");
    assert!(!list.truncated);
    let names: Vec<&str> = list.procs.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"SceShellUI"),
        "SceShellUI missing from proc_list: {names:?}"
    );
    let shell = list
        .procs
        .iter()
        .find(|p| p.name == "SceShellUI")
        .expect("SceShellUI entry");
    assert_eq!(shell.pid, 97);
    assert!(list.error.is_none());
}

/// Ports `shell_run_keeps_cwd_by_session_across_connections`.
#[test]
fn shell_run_keeps_cwd_by_session_across_calls() {
    let (n, _) = node();
    let _g = n.attach();
    let session = "shell-test-session";
    let cd = shell_run(CONSOLE, "cd /data", Some(session), Some("/"), 30).expect("cd");
    assert_eq!(cd.exit_code, Some(0));
    assert_eq!(cd.cwd.as_deref(), Some("/data"));
    assert_eq!(cd.session_id.as_deref(), Some(session));
    let pwd = shell_run(CONSOLE, "pwd", Some(session), Some("/"), 30).expect("pwd");
    assert_eq!(pwd.stdout, "/data\n");
    let ls = shell_run(CONSOLE, "ls", Some(session), Some("/"), 30).expect("ls");
    assert_eq!(ls.stdout, "listed:/data\n");
    let other = shell_run(CONSOLE, "pwd", Some("other-session"), Some("/"), 30).expect("pwd");
    assert_eq!(other.stdout, "/\n");
}

// ─── Volumes (ports volumes_integration.rs) ────────────────────────────────────

/// Ports `list_volumes_returns_default_fixtures`.
#[test]
fn list_volumes_returns_default_fixtures() {
    let (n, _) = node();
    let _g = n.attach();
    let result = list_volumes(CONSOLE).expect("list_volumes should succeed");
    assert_eq!(result.volumes.len(), 2);
    let data = result.find("/data").expect("/data present");
    assert_eq!(data.fs_type, "ufs");
    assert!(data.writable);
    assert_eq!(data.total_bytes, 800_000_000_000);
    let ext0 = result.find("/ext0").expect("/ext0 present");
    assert_eq!(ext0.free_bytes, 900_000_000_000);
}

/// Ports `list_volumes_respects_state_override`.
#[test]
fn list_volumes_respects_state_override() {
    let (n, state) = node();
    let _g = n.attach();
    *state.volumes_json.lock().unwrap() = Some(
        r#"{"volumes":[
            {"path":"/usb0","fs_type":"exfat","total_bytes":64000000000,"free_bytes":1000000000,"writable":false}
        ]}"#
        .to_string(),
    );
    let result = list_volumes(CONSOLE).expect("list_volumes should succeed");
    assert_eq!(result.volumes.len(), 1);
    let usb = result.find("/usb0").expect("/usb0 present");
    assert_eq!(usb.fs_type, "exfat");
    assert!(!usb.writable, "readonly flag must round-trip");
    assert!(result.find("/data").is_none());
}

/// Ports `list_volumes_empty`.
#[test]
fn list_volumes_empty() {
    let (n, state) = node();
    let _g = n.attach();
    *state.volumes_json.lock().unwrap() = Some(r#"{"volumes":[]}"#.to_string());
    let result = list_volumes(CONSOLE).expect("list_volumes should succeed on an empty list");
    assert!(result.volumes.is_empty());
}

/// Ports `list_volumes_surfaces_user_chosen_mount_with_tracker`: a `.ffpkg` mounted at a
/// user-chosen path surfaces with its `source_image` populated, and the field round-trips.
#[test]
fn list_volumes_surfaces_user_chosen_mount_with_tracker() {
    let (n, state) = node();
    let _g = n.attach();
    *state.volumes_json.lock().unwrap() = Some(
        r#"{"volumes":[
            {"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"nullfs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true,"is_placeholder":false},
            {"path":"/data/homebrew/PPSA17599","mount_from":"/dev/lvd0","fs_type":"ufs","total_bytes":50000000000,"free_bytes":0,"writable":true,"is_placeholder":false,"source_image":"/data/homebrew/PPSA17599.ffpkg"}
        ]}"#
        .to_string(),
    );
    let result = list_volumes(CONSOLE).expect("list_volumes should succeed");
    let mount = result
        .find("/data/homebrew/PPSA17599")
        .expect("user-chosen mount path is surfaced");
    assert_eq!(mount.source_image, "/data/homebrew/PPSA17599.ffpkg");
    assert_eq!(mount.fs_type, "ufs");
    assert!(mount.writable, "writable flag should round-trip");
    let data = result.find("/data").expect("/data present");
    assert_eq!(data.source_image, "", "non-ours mounts have empty source");
}

/// The pairing and no-listener outcomes the engine turns into the client's two dialogs, through
/// the same management seam (the transfer-side equivalents are in `ava1_transfer_integration`).
#[test]
fn a_node_with_no_management_methods_is_helper_not_ava1() {
    // An older AVA1 helper: it serves transfers but advertises no management capability.
    let dir = tempdir();
    let ava = dir.path().join("ava");
    let me = ava1::keys::Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = ava1::peers::PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ava1::server::ServerCtx::new(
        ava1::keys::Identity::generate().unwrap(),
        "old",
        peers,
        node_info_rpc(),
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let l = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = l.local_addr().unwrap().to_string();
    rt.spawn(ava1::server::serve(l, Arc::new(ctx)));
    let pool: &'static ps5upload_ava1::Pool =
        Box::leak(Box::new(ps5upload_ava1::Pool::new(ava).with_addr(addr)));
    let t = Arc::new(ps5upload_ava1::mgmt::AvaTransport::with_pool(pool));
    let _g = ps5upload_core::mgmt::scoped_transport(t);
    let e = hw_info(CONSOLE).expect_err("no management methods");
    assert!(e.to_string().contains("helper_not_ava1"), "{e}");
}
