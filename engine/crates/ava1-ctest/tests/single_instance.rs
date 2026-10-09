#![cfg(unix)]
//! Review 010 (outage 2026-10-03): the ownership record never reads as 0 for a live instance,
//! "unverifiable" never kills a bystander AND never leads to two live instances, and a second
//! helper does not open its transfer layer while the first still holds the port.
//!
//! Console-only (kept in the CUTOVER stress-test checklist): the real takeover handshake between two
//! processes, kinfo_proc ki_start on PS5 firmware (offset 336 unmeasured), and kern.boottime moving
//! with settimeofday. Everything pure or loopback is covered here.
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::time::{Duration, Instant};

use ava1_ctest::*;

#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct Rec {
    pid: c_int,
    started: u64,
    instance_id: u64,
}

extern "C" {
    fn ownership_record_format(
        buf: *mut c_char,
        n: usize,
        instance_id: u64,
        port: c_int,
        reason: c_int,
        started: u64,
        pid: c_int,
    ) -> c_int;
    fn ownership_record_parse(text: *const c_char, len: usize, r: *mut Rec) -> c_int;
    fn ownership_record_read(path: *const c_char, r: *mut Rec, retry_ms: c_int) -> c_int;
    fn ownership_record_merge(fresh: *mut Rec, snap: *const Rec);
    fn t10_reap_decision(
        ours: c_int,
        rec: u64,
        boot: u64,
        pstart: u64,
        known: c_int,
        now: u64,
    ) -> c_int;
    fn t10_gate(port: c_int, max_ms: c_int, interval_ms: c_int, starts: *mut c_int) -> c_int;
}

const REAP_YES: c_int = 0;
const REAP_NOT_OURS: c_int = 1;
const REAP_UNVERIFIABLE: c_int = 2;

fn fmt(started: u64, pid: c_int) -> Option<String> {
    let mut b = [0 as c_char; 256];
    let n = unsafe { ownership_record_format(b.as_mut_ptr(), b.len(), 77, 9120, 1, started, pid) };
    if n < 0 {
        return None;
    }
    let bytes: Vec<u8> = b[..n as usize].iter().map(|c| *c as u8).collect();
    Some(String::from_utf8(bytes).unwrap())
}

fn parse(t: &str) -> (bool, Rec) {
    let mut r = Rec::default();
    let ok = unsafe { ownership_record_parse(t.as_ptr() as *const c_char, t.len(), &mut r) } == 1;
    (ok, r)
}

#[test]
fn a_record_without_a_start_time_is_never_written() {
    assert!(
        fmt(0, 250).is_none(),
        "started_at_unix=0 must not be published"
    );
    assert!(fmt(1_700_000_000, 0).is_none());
    let t = fmt(1_700_000_000, 250).unwrap();
    let (ok, r) = parse(&t);
    assert!(ok);
    assert_eq!((r.pid, r.started, r.instance_id), (250, 1_700_000_000, 77));
}

#[test]
fn every_truncation_of_a_record_is_incomplete_never_a_wrong_number() {
    // A reader that catches the file mid-write must not turn "started_at_unix=17" into 17.
    let t = fmt(1_700_000_000, 250).unwrap();
    for cut in 0..t.len() {
        let (ok, r) = parse(&t[..cut]);
        assert!(!ok, "prefix of {cut} bytes parsed as complete");
        assert!(
            r.started == 0 || r.started == 1_700_000_000,
            "cut {cut}: {r:?}"
        );
        assert!(r.pid == 0, "pid is the last line: cut {cut}: {r:?}");
    }
    assert!(parse(&t).0);
}

#[test]
fn an_old_format_record_without_a_start_time_is_incomplete() {
    let (ok, r) = parse("instance_id=5\nruntime_port=9120\npid=250\n");
    assert!(!ok);
    assert_eq!((r.pid, r.started), (250, 0));
}

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-t10-{tag}-{}", std::process::id()))
}

fn read_path(p: &std::path::Path, retry_ms: c_int) -> (bool, Rec) {
    let c = CString::new(p.to_str().unwrap()).unwrap();
    let mut r = Rec::default();
    let ok = unsafe { ownership_record_read(c.as_ptr(), &mut r, retry_ms) } == 1;
    (ok, r)
}

#[test]
fn a_read_that_races_the_write_retries_once_and_gets_the_record() {
    // The record appears (tmp + rename, as runtime_write_ownership does) 40 ms after the first read.
    let d = tmp("race");
    let path = d.join("active_instance.txt");
    let tmp_p = d.join("active_instance.txt.tmp");
    let body = fmt(1_700_000_000, 250).unwrap();
    let (p2, t2) = (path.clone(), tmp_p.clone());
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        std::fs::write(&t2, body).unwrap();
        std::fs::rename(&t2, &p2).unwrap();
    });
    let (ok_no_retry, _) = read_path(&path, 0);
    assert!(!ok_no_retry, "precondition: the first read lost the race");
    let (ok, r) = read_path(&path, 120);
    h.join().unwrap();
    assert!(ok, "the retry must see the record: {r:?}");
    assert_eq!((r.pid, r.started), (250, 1_700_000_000));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_predecessor_that_unlinks_the_record_cannot_zero_the_start_time() {
    // The outage's mechanism: pid read, then the handing-over predecessor unlinks the record, then the
    // start time is read from a missing file = 0. The snapshot taken before the takeover fills it.
    let d = tmp("unlink");
    let path = d.join("active_instance.txt");
    std::fs::write(&path, fmt(1_700_000_000, 250).unwrap()).unwrap();
    let (ok, snap) = read_path(&path, 0);
    assert!(ok);
    std::fs::remove_file(&path).unwrap(); // the predecessor's runtime_clear_ownership
    let (ok2, mut fresh) = read_path(&path, 10);
    assert!(!ok2);
    assert_eq!(
        fresh.started, 0,
        "the lost read is what used to be treated as started=0"
    );
    unsafe { ownership_record_merge(&mut fresh, &snap) };
    assert_eq!((fresh.pid, fresh.started), (250, 1_700_000_000));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_snapshot_of_a_different_pid_is_never_mixed_in() {
    let snap = Rec {
        pid: 250,
        started: 1_700_000_000,
        instance_id: 1,
    };
    let mut fresh = Rec {
        pid: 251,
        started: 0,
        instance_id: 0,
    };
    unsafe { ownership_record_merge(&mut fresh, &snap) };
    assert_eq!((fresh.pid, fresh.started), (251, 0));
}

#[test]
fn unverifiable_never_kills_a_bystander_and_a_live_helper_is_reaped_by_the_kernel_time() {
    let (boot, now) = (1_000_000u64, 1_005_000u64);
    // The outage: record read 0, but the kernel's start time proves it is of this boot: reap it.
    assert_eq!(
        unsafe { t10_reap_decision(1, 0, boot, 1_004_000, 1, now) },
        REAP_YES
    );
    // Neither witness: leave it (a bystander must never be killed)...
    assert_eq!(
        unsafe { t10_reap_decision(1, 0, boot, 0, 0, now) },
        REAP_UNVERIFIABLE
    );
    // ...an implausible kernel time (offset wrong on this firmware, or a clock jump) falls back to the record.
    assert_eq!(
        unsafe { t10_reap_decision(1, 1_004_500, boot, 5, 1, now) },
        REAP_YES
    );
    assert_eq!(
        unsafe { t10_reap_decision(1, 0, boot, now + 3600, 1, now) },
        REAP_UNVERIFIABLE
    );
    assert_eq!(
        unsafe { t10_reap_decision(1, 0, 0, 1_004_000, 1, now) },
        REAP_UNVERIFIABLE
    );
    // A pid of a previous boot, or not one of ours.
    assert_eq!(
        unsafe { t10_reap_decision(1, 999, boot, 999, 1, now) },
        REAP_UNVERIFIABLE
    );
    assert_eq!(
        unsafe { t10_reap_decision(0, 1_004_500, boot, 1_004_000, 1, now) },
        REAP_NOT_OURS
    );
}

fn gate(port: u16, max_ms: i32) -> (bool, i32, Duration) {
    let mut starts = 0;
    let t = Instant::now();
    let started = unsafe { t10_gate(port as c_int, max_ms, 20, &mut starts) } == 1;
    (started, starts, t.elapsed())
}

#[test]
fn a_second_helper_waits_then_refuses_to_co_run_beside_a_live_receiver() {
    // The C server is one global (`ava1_server_stop` stops whichever runs): tests that start
    // it must not overlap, whatever --test-threads the runner (coverage) picks.
    let _one = CServer::lock_for_shim_tests();
    // "Unverifiable" ends here: whatever the reap decided, a prior that still answers the transfer
    // port means the new instance never starts its AVA1 side.
    let d = tmp("gate");
    let peers = CString::new(d.join("peers").to_str().unwrap()).unwrap();
    let port = unsafe {
        ffi::ava1_test_server_start(
            [7u8; 32].as_ptr(),
            peers.as_ptr(),
            &ffi::TestOpts::default(),
        )
    };
    assert!(port > 0, "receiver did not start: {port}");
    let (started, starts, took) = gate(port as u16, 400);
    assert!(
        !started && starts == 0,
        "the second instance opened its transfer layer beside a live one"
    );
    assert!(
        took >= Duration::from_millis(400),
        "it must WAIT for the takeover first, waited {took:?}"
    );
    // The first instance is still the only receiver.
    assert!(std::net::TcpStream::connect(("127.0.0.1", port as u16)).is_ok());
    unsafe { ffi::ava1_server_stop() };
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_second_helper_starts_once_the_first_lets_go_of_the_port() {
    // The C server is one global (`ava1_server_stop` stops whichever runs): tests that start
    // it must not overlap, whatever --test-threads the runner (coverage) picks.
    let _one = CServer::lock_for_shim_tests();
    let d = tmp("gate2");
    let peers = CString::new(d.join("peers").to_str().unwrap()).unwrap();
    let port = unsafe {
        ffi::ava1_test_server_start(
            [7u8; 32].as_ptr(),
            peers.as_ptr(),
            &ffi::TestOpts::default(),
        )
    };
    assert!(port > 0);
    let h = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(250));
        unsafe { ffi::ava1_server_stop() };
    });
    let (started, starts, took) = gate(port as u16, 5000);
    h.join().unwrap();
    assert!(
        started && starts == 1,
        "the gate must start exactly once after the takeover"
    );
    assert!(
        took >= Duration::from_millis(200) && took < Duration::from_millis(4000),
        "{took:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
