//! Integration tests for the new hardware-monitoring + app-lifecycle
//! RPCs against the in-process mock server.
//!
//! These catch regressions in:
//!   - FrameType <-> u16 encoding for the new frame numbers (64-71)
//!   - key=value body parsing in hw.rs (the text-format wire shape)
//!   - concurrent-request thread-safety (important because the real
//!     payload's per-mgmt-thread model exercises exactly this pattern)
//!
//! They don't touch a real PS5 -- the mock FTX2 server returns fixed
//! fixtures that mirror real PS5 readings closely enough to catch any
//! client-side parser regression.

mod mock_server;
use mock_server::MockServer;

use ps5upload_core::fs_ops::{app_launch, app_list_registered, app_register};
use ps5upload_core::hw::{app_launch_browser, hw_info, hw_power, hw_set_fan_threshold, hw_temps};

// ─── HW_INFO: static fixtures round-trip cleanly ─────────────────────

#[test]
fn hw_info_round_trip() {
    let srv = MockServer::start();
    let info = hw_info(&srv.addr).expect("hw_info");
    assert_eq!(info.model, "CFI-1215A");
    assert_eq!(info.serial, "TEST-SERIAL-123");
    assert!(info.has_wlan_bt);
    assert!(!info.has_optical_out);
    assert_eq!(info.os, "FreeBSD 11.0");
    assert_eq!(info.ncpu, 8);
    assert_eq!(info.physmem, 13_958_643_712);
}

#[test]
fn hw_temps_round_trip() {
    let srv = MockServer::start();
    let t = hw_temps(&srv.addr).expect("hw_temps");
    assert_eq!(t.cpu_temp, 65);
    assert_eq!(t.soc_temp, 72);
    assert_eq!(t.cpu_freq_mhz, 3500);
    assert_eq!(t.soc_power_mw, 85_000);
}

#[test]
fn hw_power_round_trip() {
    let srv = MockServer::start();
    let p = hw_power(&srv.addr).expect("hw_power");
    assert_eq!(p.operating_time_sec, 7230);
    assert_eq!(p.operating_time_hours, 2);
    assert_eq!(p.operating_time_minutes, 0);
}

// ─── Launch browser: plain fire-and-forget ACK ───────────────────────

#[test]
fn launch_browser_completes() {
    let srv = MockServer::start();
    app_launch_browser(&srv.addr).expect("launch_browser");
}

// ─── Concurrent stress test: simulates the Library refresh burst ─────
//
// The real payload's mgmt port now spawns a per-client thread for each
// incoming connection. That means a Library refresh opens ~8 concurrent
// TCP connections (1 list_volumes + 1 list_registered + 6 list_dir)
// that all hit the payload at once. The previous init-race bug was
// silent in single-threaded tests but lethal under this shape because
// multiple mgmt threads raced on `register_module_init` / `hw_resolve_once`.
//
// This test intentionally opens many parallel Connection threads to
// exercise the same shape. If any init on the engine/core side has a
// similar TOCTOU bug, this test will flake or panic.

#[test]
fn concurrent_hw_requests_dont_race() {
    use std::sync::Arc;
    use std::thread;

    let srv = Arc::new(MockServer::start());
    let mut handles = Vec::new();
    // 20 concurrent clients -- well above the ~8 a Library refresh
    // produces in practice. Each spawns its own TCP connection.
    for _ in 0..20 {
        let s = Arc::clone(&srv);
        handles.push(thread::spawn(move || {
            // Mix up the request types so threads compete across
            // different code paths, not just the same one.
            let _ = hw_info(&s.addr);
            let _ = hw_temps(&s.addr);
            let _ = hw_power(&s.addr);
            let _ = app_list_registered(&s.addr);
        }));
    }
    for h in handles {
        h.join().expect("worker thread should not panic");
    }
}

// ─── Register + launch round-trip ────────────────────────────────────

#[test]
fn register_then_launch_round_trip() {
    let srv = MockServer::start();
    let reg = app_register(&srv.addr, "/data/homebrew/test-game").expect("register should succeed");
    // Mock derives title_id from basename: first 5 alphanumeric chars.
    assert!(reg.title_id.starts_with("PPSA"));
    assert!(reg.used_nullfs);

    // Launch by the title_id we just got back.
    app_launch(&srv.addr, &reg.title_id).expect("launch should succeed");
}

#[test]
fn register_rejects_empty_path() {
    let srv = MockServer::start();
    let err = app_register(&srv.addr, "").expect_err("empty path must fail");
    assert!(
        err.to_string().contains("register_src_path_missing"),
        "error should surface payload-side reason: {err}"
    );
}

#[test]
fn launch_rejects_empty_title_id() {
    let srv = MockServer::start();
    let err = app_launch(&srv.addr, "").expect_err("empty title_id must fail");
    assert!(
        err.to_string().contains("launch_title_id_missing"),
        "error should surface payload-side reason: {err}"
    );
}

// ─── Fan threshold: request-path plumbing + clamp enforcement ──────

#[test]
fn fan_threshold_round_trip_in_safe_range() {
    let srv = MockServer::start();
    // Middle of the safe range — mock accepts any body.
    hw_set_fan_threshold(&srv.addr, 65).expect("fan threshold should succeed");
    // Boundaries.
    hw_set_fan_threshold(&srv.addr, 45).expect("floor should be allowed");
    hw_set_fan_threshold(&srv.addr, 80).expect("ceiling should be allowed");
}

#[test]
fn fan_threshold_rejects_below_floor() {
    let srv = MockServer::start();
    let err =
        hw_set_fan_threshold(&srv.addr, 30).expect_err("below-floor threshold must be rejected");
    assert!(
        err.to_string().contains("safe range"),
        "error should surface range info: {err}"
    );
}

#[test]
fn fan_threshold_rejects_above_ceiling() {
    let srv = MockServer::start();
    let err =
        hw_set_fan_threshold(&srv.addr, 95).expect_err("above-ceiling threshold must be rejected");
    assert!(err.to_string().contains("safe range"));
}
