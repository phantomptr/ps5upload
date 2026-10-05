#![cfg(unix)]
//! Remote Play pairing state (payload/src/rp_pair.c) over fake Sony functions.
//!
//! The two console bugs this pins (Phat, FW 13.60):
//!  - a fresh PIN read "paired" on the first status poll: the old poll fell back to
//!    sceRemoteplayGetConnectionStatus, which answered 0x80FC0001, and took any non-zero
//!    answer as paired. Now only ConfirmDeviceRegist status 2 or a grown device table is.
//!  - cancel must answer at once and leave idle even while a probe is inside Sony.
use ava1_ctest::rp_pair::{self as rp, FAILED, IDLE, PAIRED, TIMEOUT, WAITING, WAIT_MS};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The C side is one shared instance.
static SERIAL: Mutex<()> = Mutex::new(());

fn fresh() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    rp::reset();
    g
}

const T0: i64 = 1_000_000;

#[test]
fn a_new_pin_waits_with_a_countdown() {
    let _g = fresh();
    rp::set_gen(0, 36876659);
    assert_eq!(rp::request(T0), 0);
    let v = rp::view(T0);
    assert_eq!(v.state, WAITING);
    assert_eq!(v.pin, "36876659");
    assert_eq!(v.seconds_left, 300);
    // Confirm says 0 (nobody registered yet), as measured on the console.
    rp::set_confirm(0, 0, 0);
    assert_eq!(rp::poll(T0 + 3_000), WAITING);
    let v = rp::view(T0 + 3_000);
    assert_eq!(v.state, WAITING);
    assert_eq!(v.seconds_left, 297);
    assert_eq!(v.probes, 1);
    // The old PIN is cleared before the new one is made, once the module is up.
    let c = rp::counts();
    assert_eq!((c.prepare, c.gen_pin, c.invalidate), (1, 1, 1));
}

#[test]
fn pins_keep_their_leading_zeros() {
    let _g = fresh();
    rp::set_gen(0, 450588);
    rp::request(T0);
    assert_eq!(rp::view(T0).pin, "00450588");
}

#[test]
fn a_probe_error_or_an_unknown_status_is_not_pairing() {
    let _g = fresh();
    rp::request(T0);
    // The bug-1 shape: an error code from a Sony call must never read as paired.
    rp::set_confirm(0x80FC0001u32 as i32, 0, 0);
    assert_eq!(rp::poll(T0 + 1_000), WAITING);
    rp::set_confirm(0, 1, 0);
    assert_eq!(rp::poll(T0 + 2_000), WAITING);
    assert_eq!(rp::view(T0 + 2_000).state, WAITING);
}

#[test]
fn confirm_status_2_is_paired() {
    let _g = fresh();
    rp::request(T0);
    rp::poll(T0 + 1_000);
    rp::set_confirm(0, 2, 0);
    assert_eq!(rp::poll(T0 + 2_000), PAIRED);
    let v = rp::view(T0 + 2_000);
    assert_eq!(v.state, PAIRED);
    assert_eq!(v.pin, "", "a used PIN is not shown again");
    assert_eq!(v.seconds_left, 0);
    assert!(rp::last_notify().contains("paired"));
    // Paired is final: later polls do not probe Sony again.
    let before = rp::counts().confirm;
    assert_eq!(rp::poll(T0 + 3_000), PAIRED);
    assert_eq!(rp::counts().confirm, before);
}

#[test]
fn a_grown_device_table_is_paired_too() {
    let _g = fresh();
    rp::set_devices(1);
    rp::request(T0);
    rp::set_confirm(0, 0, 0);
    assert_eq!(rp::poll(T0 + 1_000), WAITING);
    rp::set_devices(2);
    assert_eq!(rp::poll(T0 + 2_000), PAIRED);
}

#[test]
fn an_unreadable_device_table_is_not_growth() {
    let _g = fresh();
    rp::set_devices(-1);
    rp::request(T0);
    rp::set_devices(3);
    assert_eq!(rp::poll(T0 + 1_000), WAITING);
}

#[test]
fn a_wrong_pin_fails_with_the_reason() {
    let _g = fresh();
    rp::request(T0);
    rp::set_confirm(0, 3, 0x80FC1047);
    assert_eq!(rp::poll(T0 + 1_000), FAILED);
    let v = rp::view(T0 + 1_000);
    assert!(v.err.contains("PIN was entered wrong"), "{}", v.err);
    assert!(v.err.contains("0x80FC1047"), "{}", v.err);
    assert_eq!(v.pin, "");
}

#[test]
fn status_4_fails_as_ended_by_the_console() {
    let _g = fresh();
    rp::request(T0);
    rp::set_confirm(0, 4, 0);
    assert_eq!(rp::poll(T0 + 1_000), FAILED);
    assert!(rp::view(T0 + 1_000).err.contains("ended the registration"));
}

#[test]
fn the_pin_expires_into_timeout_and_is_invalidated() {
    let _g = fresh();
    rp::request(T0);
    let inv = rp::counts().invalidate;
    assert_eq!(rp::poll(T0 + WAIT_MS - 1), WAITING);
    assert_eq!(rp::view(T0 + WAIT_MS - 1).seconds_left, 1);
    assert_eq!(rp::poll(T0 + WAIT_MS), TIMEOUT);
    let v = rp::view(T0 + WAIT_MS);
    assert_eq!(v.state, TIMEOUT);
    assert_eq!(v.seconds_left, 0);
    assert_eq!(v.pin, "");
    assert!(v.err.contains("expired"), "{}", v.err);
    assert_eq!(
        rp::counts().invalidate,
        inv + 1,
        "an expired PIN is invalidated once"
    );
    // and a late confirm cannot turn it into paired
    rp::set_confirm(0, 2, 0);
    assert_eq!(rp::poll(T0 + WAIT_MS + 5_000), TIMEOUT);
    assert_eq!(rp::counts().invalidate, inv + 1);
}

#[test]
fn expiry_wins_over_a_confirm_in_the_same_poll() {
    let _g = fresh();
    rp::request(T0);
    rp::set_confirm(0, 2, 0);
    assert_eq!(rp::poll(T0 + WAIT_MS + 1), TIMEOUT);
}

#[test]
fn cancel_goes_idle_and_owes_one_invalidation() {
    let _g = fresh();
    rp::request(T0);
    rp::poll(T0 + 500);
    let inv = rp::counts().invalidate;
    assert!(rp::cancel(), "a live PIN has to be invalidated");
    let v = rp::view(T0 + 1_000);
    assert_eq!(v.state, IDLE);
    assert_eq!(v.pin, "");
    assert_eq!(v.seconds_left, 0);
    assert_eq!(v.probes, 0, "the last PIN's diagnostics go with it");
    assert_eq!(
        rp::counts().invalidate,
        inv,
        "cancel itself makes no Sony call"
    );
    rp::settle();
    rp::settle();
    assert_eq!(rp::counts().invalidate, inv + 1, "settled exactly once");
}

#[test]
fn an_owed_invalidation_is_settled_by_the_next_poll() {
    let _g = fresh();
    rp::request(T0);
    let inv = rp::counts().invalidate;
    rp::cancel();
    assert_eq!(rp::poll(T0 + 1_000), IDLE);
    assert_eq!(rp::counts().invalidate, inv + 1);
    // and idle polls do not probe
    assert_eq!(rp::counts().confirm, 0);
}

#[test]
fn cancel_when_nothing_is_live_owes_nothing() {
    let _g = fresh();
    assert!(!rp::cancel());
    rp::request(T0);
    rp::set_confirm(0, 2, 0);
    rp::poll(T0 + 1_000);
    assert!(!rp::cancel(), "a used PIN is not invalidated");
    assert_eq!(rp::view(T0 + 2_000).state, IDLE);
}

#[test]
fn cancel_returns_while_a_probe_is_stuck_in_sony_and_its_answer_is_dropped() {
    let _g = fresh();
    rp::request(T0);
    rp::set_confirm(0, 2, 0);
    rp::block_confirm(true);
    let probe = std::thread::spawn(|| rp::poll(T0 + 1_000));
    assert!(rp::wait_in_confirm(2_000), "the probe reached Sony");
    let t = Instant::now();
    assert!(rp::cancel());
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "cancel waited on Sony"
    );
    assert_eq!(rp::view(T0 + 1_000).state, IDLE);
    rp::block_confirm(false);
    assert_eq!(probe.join().unwrap(), IDLE);
    let v = rp::view(T0 + 2_000);
    assert_eq!(v.state, IDLE, "a late 'paired' must not undo the cancel");
    assert_eq!(v.probes, 0);
}

#[test]
fn a_new_request_replaces_the_live_pin() {
    let _g = fresh();
    rp::set_gen(0, 11111111);
    rp::request(T0);
    rp::set_gen(0, 22222222);
    assert_eq!(rp::request(T0 + 60_000), 0);
    let v = rp::view(T0 + 60_000);
    assert_eq!(v.state, WAITING);
    assert_eq!(v.pin, "22222222");
    assert_eq!(v.seconds_left, 300, "the deadline restarts");
    assert_eq!(rp::counts().invalidate, 2, "the old PIN was invalidated");
}

#[test]
fn a_request_after_timeout_or_failure_starts_clean() {
    let _g = fresh();
    rp::request(T0);
    rp::poll(T0 + WAIT_MS);
    assert_eq!(rp::request(T0 + WAIT_MS + 1), 0);
    let v = rp::view(T0 + WAIT_MS + 1);
    assert_eq!((v.state, v.err.as_str(), v.probes), (WAITING, "", 0));
}

#[test]
fn prepare_and_generate_failures_are_failed_with_the_cause() {
    let _g = fresh();
    rp::set_prepare(-1);
    assert_eq!(rp::request(T0), -1);
    let v = rp::view(T0);
    assert_eq!(v.state, FAILED);
    assert_eq!(v.err, "fake prepare refused");
    assert_eq!(rp::counts().gen_pin, 0);

    rp::set_prepare(0);
    rp::set_gen(0x80FC0002u32 as i32, 0);
    assert_eq!(rp::request(T0), -1);
    let v = rp::view(T0);
    assert_eq!(v.state, FAILED);
    assert!(v.err.contains("0x80FC0002"), "{}", v.err);
    assert_eq!(v.pin, "");
}

#[test]
fn state_names_are_the_wire_names() {
    let _g = fresh();
    let names: Vec<String> = (0..5).map(rp::state_name).collect();
    assert_eq!(names, ["idle", "waiting", "paired", "failed", "timeout"]);
    assert_eq!(rp::state_name(9), "unknown");
}
