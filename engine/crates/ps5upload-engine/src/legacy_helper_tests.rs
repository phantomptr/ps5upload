//! Tests of the migration shim (`legacy_helper.rs`); deleted with it in the release after the cutover.
use super::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A fake old helper on a loopback management port: answers `Hello` with a header, records
/// every `Shutdown` frame's bytes, acknowledges it, and (when `exits`) stops listening.
struct Fake {
    ports: Ports,
    seen: Arc<Mutex<Vec<[u8; HEADER_LEN]>>>,
    closed: Arc<AtomicBool>,
}

/// What a build from before the cutover answers to `Hello`: no AVA1 field.
const OLD_HELLO: &str = r#"{"version":1,"instance_id":5,"runtime_port":9113}"#;

fn fake_helper(exits: bool) -> Fake {
    fake_helper_with(exits, OLD_HELLO)
}

fn fake_helper_with(exits: bool, hello_body: &'static str) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let ports = Ports {
        mgmt: l.local_addr().unwrap().port(),
        transfer: free_port(),
        ava1: free_port(),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let (s2, c2) = (seen.clone(), closed.clone());
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut h = [0u8; HEADER_LEN];
            if s.read_exact(&mut h).is_err() {
                continue; // a bare connect (the port check)
            }
            let ty = u16::from_le_bytes([h[6], h[7]]);
            s2.lock().unwrap().push(h);
            let reply = if ty == SHUTDOWN { SHUTDOWN_ACK } else { 2 };
            let mut out = frame(reply).to_vec();
            let body = if ty == HELLO { hello_body } else { "{}" };
            out[12..20].copy_from_slice(&(body.len() as u64).to_le_bytes());
            out.extend_from_slice(body.as_bytes());
            let _ = s.write_all(&out);
            if ty == SHUTDOWN && exits {
                c2.store(true, Ordering::SeqCst);
                return; // drops the listener: the port closes
            }
        }
    });
    Fake {
        ports,
        seen,
        closed,
    }
}

#[test]
fn frame_bytes_are_the_old_header() {
    let mut want = [0u8; HEADER_LEN];
    want[..4].copy_from_slice(b"FTX2");
    want[4] = 1;
    want[6] = 22;
    assert_eq!(frame(SHUTDOWN), want);
    assert_eq!(frame(HELLO)[6], 1);
}

#[test]
fn probe_recognises_an_old_helper_and_nothing_else() {
    let fake = fake_helper(false);
    assert!(probe("127.0.0.1", fake.ports));
    // a listener that is not an old helper never answers with the magic
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = Ports {
        mgmt: other.local_addr().unwrap().port(),
        transfer: free_port(),
        ava1: 0,
    };
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = other.accept() {
            let _ = s.write_all(&[0u8; HEADER_LEN]);
        }
    });
    assert!(!probe("127.0.0.1", p));
    // nothing at all
    let none = Ports {
        mgmt: free_port(),
        transfer: free_port(),
        ava1: 0,
    };
    assert!(!probe("127.0.0.1", none));
}

const NEW_STARTING: &str =
    r#"{"version":1,"instance_id":9,"runtime_port":9113,"ava1_port":9120,"ava1":"starting"}"#;
const NEW_FAILED: &str =
    r#"{"version":1,"instance_id":9,"runtime_port":9113,"ava1_port":9120,"ava1":"failed"}"#;

/// A NEW helper still serves the old ports. It is not "old": while its AVA1 server is coming up it
/// is `starting`, and when that server never started it is `ava1_failed`. Only a build whose Hello
/// names no AVA1 port is `helper_old`, the one state a replace is offered for.
#[test]
fn a_new_build_is_never_called_helper_old() {
    let old = fake_helper(false);
    assert_eq!(state("127.0.0.1", old.ports), HELPER_OLD);
    let booting = fake_helper_with(false, NEW_STARTING);
    assert_eq!(state("127.0.0.1", booting.ports), STARTING);
    let failed = fake_helper_with(false, NEW_FAILED);
    assert_eq!(state("127.0.0.1", failed.ports), AVA1_FAILED);
    // once its AVA1 port listens it is simply AVA1, whatever it still says on the old ports
    let up = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = Ports {
        ava1: up.local_addr().unwrap().port(),
        ..booting.ports
    };
    assert_eq!(state("127.0.0.1", p), AVA1);
}

#[test]
fn build_of_reads_the_hello_reply() {
    assert_eq!(build_of(OLD_HELLO.as_bytes()), Build::Old);
    assert_eq!(
        build_of(b""),
        Build::Old,
        "a header-only reply is an old build"
    );
    assert_eq!(build_of(b"not json"), Build::Old);
    assert_eq!(build_of(NEW_FAILED.as_bytes()), Build::New("failed".into()));
}

#[test]
fn state_tells_the_three_situations_apart() {
    let fake = fake_helper(false);
    assert_eq!(state("127.0.0.1", fake.ports), HELPER_OLD);
    let none = Ports {
        mgmt: free_port(),
        transfer: free_port(),
        ava1: free_port(),
    };
    assert_eq!(state("127.0.0.1", none), NOT_RUNNING);
    let up = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = Ports {
        ava1: up.local_addr().unwrap().port(),
        ..fake.ports
    };
    assert_eq!(
        state("127.0.0.1", p),
        AVA1,
        "AVA1 wins over a leftover old listener"
    );
}

#[test]
fn legacy_helper_is_shut_down_then_replaced() {
    let fake = fake_helper(true);
    let ava1 = fake.ports.ava1;
    let closed = fake.closed.clone();
    let sent = Arc::new(Mutex::new(None::<bool>));
    let sent2 = sent.clone();
    let new_helper = Arc::new(Mutex::new(None::<TcpListener>));
    let nh = new_helper.clone();
    let r = replace(
        "127.0.0.1",
        fake.ports,
        Duration::from_secs(5),
        Duration::from_secs(5),
        move || {
            // the send step: the old helper must be gone by now
            *sent2.lock().unwrap() = Some(closed.load(Ordering::SeqCst));
            *nh.lock().unwrap() = Some(TcpListener::bind(("127.0.0.1", ava1)).unwrap());
            Ok(())
        },
    )
    .expect("replaced");
    assert_eq!(r, Replaced { ava1_up: true });
    assert_eq!(
        *sent.lock().unwrap(),
        Some(true),
        "sent after the old helper exited"
    );
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen.last().copied(),
        Some(frame(SHUTDOWN)),
        "the shutdown frame bytes"
    );
}

#[test]
fn legacy_helper_wedged_is_reported() {
    let fake = fake_helper(false); // acknowledges, never lets go of its port
    let called = Arc::new(AtomicBool::new(false));
    let c2 = called.clone();
    let r = replace(
        "127.0.0.1",
        fake.ports,
        Duration::from_millis(400),
        Duration::from_millis(100),
        move || {
            c2.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert_eq!(r, Err(ReplaceError::Wedged));
    assert!(
        !called.load(Ordering::SeqCst),
        "the new helper is not sent over a live one"
    );
    assert!(r.unwrap_err().to_string().starts_with(LEGACY_HELPER_WEDGED));
}

#[test]
fn a_failed_send_is_not_called_wedged() {
    let fake = fake_helper(true);
    let r = replace(
        "127.0.0.1",
        fake.ports,
        Duration::from_secs(5),
        Duration::from_millis(100),
        || Err("connect 10.0.0.2:9021: refused".into()),
    );
    assert_eq!(
        r,
        Err(ReplaceError::Send("connect 10.0.0.2:9021: refused".into()))
    );
}

#[test]
fn a_new_helper_that_is_slow_to_listen_is_not_an_error() {
    let fake = fake_helper(true);
    let r = replace(
        "127.0.0.1",
        fake.ports,
        Duration::from_secs(5),
        Duration::from_millis(200),
        || Ok(()),
    );
    assert_eq!(r, Ok(Replaced { ava1_up: false }));
}

/// The status probe's failure carries the helper state, so an older helper reads `helper_old`
/// (the banner's Update) instead of the generic `helper_not_ava1` ("down").
#[test]
fn a_status_failure_names_an_older_helper() {
    let err = "payload rejected NODE_STATUS: helper_not_ava1: not running".to_string();
    let old = fake_helper(false);
    let out = fold_status_error(err.clone(), "127.0.0.1", old.ports);
    assert!(out.starts_with("helper_old:"), "{out}");
    let booting = fake_helper_with(false, NEW_STARTING);
    let out = fold_status_error(err.clone(), "127.0.0.1", booting.ports);
    assert!(out.starts_with("helper_starting:"), "{out}");
    let failed = fake_helper_with(false, NEW_FAILED);
    let out = fold_status_error(err.clone(), "127.0.0.1", failed.ports);
    assert!(out.starts_with("ava1_failed:"), "{out}");
    // nothing answers: the original error, untouched
    let none = Ports {
        mgmt: free_port(),
        transfer: free_port(),
        ava1: free_port(),
    };
    assert_eq!(fold_status_error(err.clone(), "127.0.0.1", none), err);
}

#[test]
fn other_status_failures_are_not_probed() {
    // Any other failure (not paired, a timeout) is returned as is, without touching the console.
    let none = Ports {
        mgmt: free_port(),
        transfer: free_port(),
        ava1: free_port(),
    };
    for e in ["not_paired: pair first", "timed out"] {
        assert_eq!(fold_status_error(e.into(), "127.0.0.1", none), e);
    }
}
