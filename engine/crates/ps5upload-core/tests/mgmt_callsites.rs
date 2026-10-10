//! The migrated call sites against a fake transport: each keeps its public signature and its
//! legacy request and reply bodies, and the seam carries the AVA1 method and the label.

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

#[test]
fn hw_info_parses_the_text_reply_it_asked_for() {
    let (t, _g) = fake(|_, _| reply(b"model=PS5 Pro\nserial=ABC\nncpu=8\n"));
    let i = ps5upload_core::hw::hw_info("10.0.0.5").unwrap();
    assert_eq!(
        (i.model.as_str(), i.serial.as_str(), i.ncpu),
        ("PS5 Pro", "ABC", 8)
    );
    let seen = t.seen.lock().unwrap();
    assert_eq!((seen[0].0, seen[0].1.as_str()), (72, "HW_INFO"));
}

#[test]
fn hw_temps_sends_its_selector_as_the_body() {
    let (t, _g) = fake(|_, _| reply(b"cpu_temp=50\n"));
    ps5upload_core::hw::hw_temps("a:1", true).unwrap();
    assert_eq!(t.seen.lock().unwrap()[0].2, b"ufs");
}

#[test]
fn list_dir_sends_the_legacy_json_with_its_label_and_deadline() {
    let (t, _g) = fake(|_, _| {
        reply(br#"{"path":"/data","entries":[{"name":"a","kind":"file","size":3,"mtime":4}],"truncated":false,"total_scanned":1,"returned":1}"#)
    });
    let opts = ps5upload_core::fs_ops::ListDirOptions {
        offset: 2,
        limit: 50,
    };
    let l = ps5upload_core::fs_ops::list_dir_with_timeout(
        "a:1",
        "/data",
        opts,
        Some(Duration::from_secs(4)),
    )
    .unwrap();
    assert_eq!(l.entries[0].name, "a");
    let seen = t.seen.lock().unwrap();
    assert_eq!(seen[0].0, 33);
    assert_eq!(seen[0].1, "FS_LIST_DIR(/data)");
    assert_eq!(seen[0].3, Duration::from_secs(4));
    let v: serde_json::Value = serde_json::from_slice(&seen[0].2).unwrap();
    assert_eq!(
        (
            v["path"].as_str(),
            v["offset"].as_u64(),
            v["limit"].as_u64()
        ),
        (Some("/data"), Some(2), Some(50))
    );
}

#[test]
fn a_list_refusal_reads_exactly_as_it_did() {
    let (_t, _g) = fake(|_, _| {
        Err(MgmtError {
            label: "FS_LIST_DIR(/x)".into(),
            status: 9,
            cause: "fs_list_dir_path_not_allowed".into(),
        }
        .into())
    });
    let e = ps5upload_core::fs_ops::list_dir("a:1", "/x", Default::default()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "payload rejected FS_LIST_DIR(/x): fs_list_dir_path_not_allowed"
    );
}

#[test]
fn fs_read_returns_the_raw_bytes_and_passes_the_unsafe_flag() {
    let (t, _g) = fake(|_, _| reply(&[1, 2, 3]));
    let r = ps5upload_core::fs_ops::fs_read_with_timeout("a:1", "/system/x", 10, 3, None, true)
        .unwrap();
    assert_eq!(r, [1, 2, 3]);
    let seen = t.seen.lock().unwrap();
    assert_eq!((seen[0].0, seen[0].1.as_str()), (38, "FS_READ(/system/x)"));
    let v: serde_json::Value = serde_json::from_slice(&seen[0].2).unwrap();
    assert_eq!(
        (
            v["unsafe"].as_bool(),
            v["offset"].as_u64(),
            v["limit"].as_u64()
        ),
        (Some(true), Some(10), Some(3))
    );
}

#[test]
fn fs_mkdir_calls_the_typed_method_with_the_legacy_body() {
    let (t, _g) = fake(|_, _| reply(b""));
    ps5upload_core::fs_ops::fs_mkdir("a:1", "/data/new").unwrap();
    let seen = t.seen.lock().unwrap();
    assert_eq!(seen[0].0, m::FS_MKDIR.id);
    assert_eq!(seen[0].2, br#"{"path":"/data/new"}"#);
}

#[test]
fn fs_write_bytes_succeeds_and_turns_a_refusal_into_the_old_failure_text() {
    let (t, _g) = fake(|_, _| reply(br#"{"ok":true,"size":3}"#));
    let r = ps5upload_core::diagnostics::fs_write_bytes("a:1", "/data/x", b"abc", true).unwrap();
    assert_eq!(r.size, Some(3));
    {
        let seen = t.seen.lock().unwrap();
        assert_eq!((seen[0].0, seen[0].1.as_str()), (39, "FS_WRITE_BYTES"));
        let v: serde_json::Value = serde_json::from_slice(&seen[0].2).unwrap();
        assert_eq!(v["mode"], "create");
    }
    drop(_g);
    let (_t, _g) = fake(|_, _| {
        Err(MgmtError {
            label: "FS_WRITE_BYTES".into(),
            status: 14,
            cause: "exists".into(),
        }
        .into())
    });
    let e =
        ps5upload_core::diagnostics::fs_write_bytes("a:1", "/data/x", b"abc", true).unwrap_err();
    assert_eq!(e.to_string(), "FS_WRITE_BYTES failed: exists");
}

#[test]
fn the_health_scan_reads_the_rebuilt_status_json() {
    let (t, _g) = fake(|method, _| {
        if method.id == 4 {
            reply(br#"{"version":"5.42.0","ucred_elevated":true,"prior_instance":"replaced"}"#)
        } else {
            Err(anyhow::anyhow!("not answered in this test"))
        }
    });
    let report = ps5upload_core::health::run_health_scan("a:1", "5.42.0");
    let c = report
        .checks
        .iter()
        .find(|c| c.id == "payload_version_match")
        .expect("version check");
    assert!(format!("{:?}", c.status).contains("Pass"), "{c:?}");
    assert_eq!(t.seen.lock().unwrap()[0].0, 4);
}

#[test]
fn power_actions_do_not_report_success_when_the_engine_was_never_reached() {
    use ps5upload_core::mgmt_proxy::ForwardError;
    use ps5upload_core::system_control::{system_control, PowerAction};
    // The forwarder could not reach the engine (or the engine refused the hop with a 403):
    // nothing was sent to the console, so Reboot/Shutdown/Standby must fail.
    for action in [
        PowerAction::Reboot,
        PowerAction::Shutdown,
        PowerAction::Standby,
    ] {
        let (_t, _g) = fake(|_, _| {
            Err(anyhow::Error::new(ForwardError(
                "engine refused the management call (403 Forbidden): loopback only".into(),
            )))
        });
        let e = system_control("a:1", action).unwrap_err();
        assert!(
            e.downcast_ref::<ForwardError>().is_some(),
            "{action:?}: {e}"
        );
    }
    // A connection the console dropped after the request is still the expected success.
    let (_t, _g) = fake(|_, _| Err(anyhow::anyhow!("connection reset by peer")));
    assert!(system_control("a:1", PowerAction::Reboot).unwrap().ok);
}
