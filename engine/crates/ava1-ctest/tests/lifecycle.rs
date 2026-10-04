#![cfg(unix)]
//! P3 Task 8: the payload's side of takeover. The old binary protocol lives only in
//! payload/src/legacy_takeover.c (a migration shim); between AVA1-era instances the new one
//! writes a flag file the old one polls (payload/src/takeover_flag.c).
use std::ffi::CString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Links against ava1c (build.rs), which compiles the two payload files.
use ava1_ctest as _;

extern "C" {
    fn legacy_takeover_frame(hdr: *mut u8);
    fn legacy_takeover(
        mgmt: c_int,
        xfer: c_int,
        ack_s: c_int,
        attempts: c_int,
        interval_us: c_int,
    ) -> c_int;
    fn takeover_flag_write(dir: *const c_char, nonce: u64) -> c_int;
    fn takeover_flag_read(dir: *const c_char, nonce: *mut u64) -> c_int;
    fn takeover_flag_unlink(dir: *const c_char);
    fn takeover_nonce_new(nonce: *mut u64) -> c_int;
    fn takeover_flag_identity(dir: *const c_char, out: *mut Id);
    fn takeover_flag_asks_us_to_exit(dir: *const c_char, my: u64, stale: *const Id) -> c_int;
    fn takeover_flag_request(
        dir: *const c_char,
        nonce: u64,
        ports: *const c_int,
        n: c_int,
        attempts: c_int,
        interval_us: c_int,
    ) -> c_int;
    fn takeover_wait_port_free(port: c_int, max_ms: c_int, interval_ms: c_int) -> c_int;
    fn takeover_flag_poll_start(
        dir: *const c_char,
        nonce: u64,
        period_ms: c_int,
        cb: extern "C" fn(),
    ) -> c_int;
}

/// Mirrors takeover_flag_id_t.
#[repr(C)]
#[derive(Default)]
struct Id {
    present: c_int,
    nonce: u64,
    ino: u64,
    mtime_ns: i64,
}

const NONE: c_int = 0;
const FREED: c_int = 1;
const STUCK: c_int = -1;

fn dir() -> (tempdir::Dir, CString) {
    let d = tempdir::Dir::new();
    let c = CString::new(d.path().to_str().unwrap()).unwrap();
    (d, c)
}

/// A scratch directory that removes itself (no tempfile dependency in this crate).
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "ava1-t8-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn c_legacy_takeover_frame_bytes_match_the_ftx2_header() {
    // payload/src/takeover.c before the cutover: 28 bytes (the plan said 24; the code is the
    // authority): magic "FTX2" LE, version 1, frame type 18, flags 0, body_len 0, trace_id 0.
    let mut h = [0xEEu8; 28];
    unsafe { legacy_takeover_frame(h.as_mut_ptr()) };
    let mut want = [0u8; 28];
    want[0..4].copy_from_slice(&0x3258_5446u32.to_le_bytes());
    want[4..6].copy_from_slice(&1u16.to_le_bytes());
    want[6..8].copy_from_slice(&18u16.to_le_bytes());
    assert_eq!(h, want);
}

/// An "old helper": reads the request, checks it, answers with a 28-byte reply and exits (the
/// listener closes with the thread).
fn old_helper(l: TcpListener, got: Arc<AtomicBool>, answer: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut b = [0u8; 28];
        s.read_exact(&mut b).unwrap();
        let mut want = [0u8; 28];
        unsafe { legacy_takeover_frame(want.as_mut_ptr()) };
        assert_eq!(b, want);
        got.store(true, Ordering::SeqCst);
        if answer {
            s.write_all(&[0u8; 28]).unwrap();
        }
        // dropping `l` and `s` frees the port
    })
}

#[test]
fn legacy_takeover_asks_the_old_helper_and_waits_for_its_ports() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let mgmt = l.local_addr().unwrap().port();
    let got = Arc::new(AtomicBool::new(false));
    let h = old_helper(l, got.clone(), true);
    let rc = unsafe { legacy_takeover(mgmt as c_int, free_port() as c_int, 2, 50, 20_000) };
    h.join().unwrap();
    assert!(got.load(Ordering::SeqCst));
    assert_eq!(rc, FREED);
}

#[test]
fn legacy_takeover_with_no_old_helper_is_none() {
    let rc = unsafe { legacy_takeover(free_port() as c_int, free_port() as c_int, 1, 5, 1000) };
    assert_eq!(rc, NONE);
}

#[test]
fn legacy_takeover_reports_a_helper_that_does_not_exit() {
    // Accepts and answers, but never lets go of its port.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let mgmt = l.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let t = std::thread::spawn(move || {
        l.set_nonblocking(true).unwrap();
        while !s2.load(Ordering::SeqCst) {
            if let Ok((mut s, _)) = l.accept() {
                s.set_nonblocking(false).ok();
                let mut b = [0u8; 28];
                let _ = s.read(&mut b);
                let _ = s.write_all(&[0u8; 28]);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let rc = unsafe { legacy_takeover(mgmt as c_int, free_port() as c_int, 1, 5, 10_000) };
    stop.store(true, Ordering::SeqCst);
    t.join().unwrap();
    assert_eq!(rc, STUCK);
}

fn flag_path(c: &CString) -> std::path::PathBuf {
    std::path::Path::new(c.to_str().unwrap()).join("takeover")
}

#[test]
fn flag_file_roundtrip_and_who_it_asks_to_exit() {
    let (_d, c) = dir();
    let mut n = 0u64;
    assert_eq!(
        unsafe { takeover_flag_read(c.as_ptr(), &mut n) },
        -1,
        "absent"
    );
    assert_eq!(
        unsafe { takeover_flag_asks_us_to_exit(c.as_ptr(), 1, std::ptr::null()) },
        0
    );
    assert_eq!(unsafe { takeover_flag_write(c.as_ptr(), 500) }, 0);
    assert_eq!(unsafe { takeover_flag_read(c.as_ptr(), &mut n) }, 0);
    assert_eq!(n, 500);
    // a different instance's nonce asks us to exit; our own never does
    assert_eq!(
        unsafe { takeover_flag_asks_us_to_exit(c.as_ptr(), 499, std::ptr::null()) },
        1
    );
    assert_eq!(
        unsafe { takeover_flag_asks_us_to_exit(c.as_ptr(), 500, std::ptr::null()) },
        0
    );
    // what was already there at our start (the stale identity) does not
    let mut id = Id::default();
    unsafe { takeover_flag_identity(c.as_ptr(), &mut id) };
    assert_eq!(id.present, 1);
    assert_eq!(
        unsafe { takeover_flag_asks_us_to_exit(c.as_ptr(), 499, &id) },
        0
    );
    // garbage is not a request
    std::fs::write(flag_path(&c), b"zzz").unwrap();
    assert_eq!(
        unsafe { takeover_flag_asks_us_to_exit(c.as_ptr(), 1, std::ptr::null()) },
        0
    );
    unsafe { takeover_flag_unlink(c.as_ptr()) };
    assert!(!flag_path(&c).exists());
    assert!(!std::path::Path::new(c.to_str().unwrap())
        .join("takeover.tmp")
        .exists());
}

#[test]
fn nonces_are_random_and_never_zero() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let mut n = 0u64;
        assert_eq!(unsafe { takeover_nonce_new(&mut n) }, 0);
        assert_ne!(n, 0);
        assert!(seen.insert(n), "a repeated nonce");
    }
}

static OLD_EXITED: AtomicBool = AtomicBool::new(false);
extern "C" fn old_exits() {
    OLD_EXITED.store(true, Ordering::SeqCst);
}

#[test]
fn flag_file_takeover_exits_the_old_instance() {
    let (_d, c) = dir();
    // The old instance (nonce 100) serves on `port` and polls the flag every 20 ms.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port() as c_int;
    l.set_nonblocking(true).unwrap();
    assert_eq!(
        unsafe { takeover_flag_poll_start(c.as_ptr(), 100, 20, old_exits) },
        0
    );
    let old = std::thread::spawn(move || {
        // "serves" until its poll thread says exit, then releases the port
        let t0 = Instant::now();
        while !OLD_EXITED.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(10) {
            let _ = l.accept();
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(l);
    });
    assert!(TcpStream::connect(("127.0.0.1", port as u16)).is_ok());
    // The new instance (nonce 200) asks and waits for the port to free.
    let ports = [port];
    let rc = unsafe { takeover_flag_request(c.as_ptr(), 200, ports.as_ptr(), 1, 200, 20_000) };
    old.join().unwrap();
    assert_eq!(rc, 0, "the port freed");
    assert!(OLD_EXITED.load(Ordering::SeqCst), "the old instance exited");
    assert!(
        !flag_path(&c).exists(),
        "the flag is removed after a successful takeover"
    );
}

#[test]
fn flag_file_takeover_times_out_on_an_instance_that_stays() {
    let (_d, c) = dir();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port() as c_int;
    let ports = [port];
    let rc = unsafe { takeover_flag_request(c.as_ptr(), 200, ports.as_ptr(), 1, 5, 5_000) };
    assert_eq!(rc, -1);
    assert!(
        flag_path(&c).exists(),
        "the flag stays for the old instance"
    );
    drop(l);
}

static STALE_RAN: AtomicBool = AtomicBool::new(false);
extern "C" fn stale_cb() {
    STALE_RAN.store(true, Ordering::SeqCst);
}

#[test]
fn a_stale_flag_at_startup_does_nothing() {
    let (_d, c) = dir();
    // left behind by an instance that died or by a reboot
    unsafe { takeover_flag_write(c.as_ptr(), 7) };
    assert_eq!(
        unsafe { takeover_flag_poll_start(c.as_ptr(), 9, 10, stale_cb) },
        0
    );
    assert!(
        !flag_path(&c).exists(),
        "the poll removed the stale flag at start"
    );
    std::thread::sleep(Duration::from_millis(250));
    assert!(!STALE_RAN.load(Ordering::SeqCst));
}

static OWN_RAN: AtomicBool = AtomicBool::new(false);
extern "C" fn own_cb() {
    OWN_RAN.store(true, Ordering::SeqCst);
}

#[test]
fn our_own_flag_never_stops_us() {
    let (_d, c) = dir();
    assert_eq!(
        unsafe { takeover_flag_poll_start(c.as_ptr(), 41, 10, own_cb) },
        0
    );
    unsafe { takeover_flag_write(c.as_ptr(), 41) };
    std::thread::sleep(Duration::from_millis(250));
    assert!(!OWN_RAN.load(Ordering::SeqCst));
}

static CLOCK_RAN: AtomicBool = AtomicBool::new(false);
extern "C" fn clock_cb() {
    CLOCK_RAN.store(true, Ordering::SeqCst);
}

extern "C" {
    fn utimes(path: *const c_char, times: *const [i64; 4]) -> c_int;
}

/// The app sets the console clock with settimeofday, so a fresh flag can carry an mtime far in the
/// past or future. The poll never orders by time, so a fresh flag still works whatever its mtime.
#[test]
fn a_backward_clock_does_not_matter() {
    let (_d, c) = dir();
    assert_eq!(
        unsafe { takeover_flag_poll_start(c.as_ptr(), 51, 10, clock_cb) },
        0
    );
    unsafe { takeover_flag_write(c.as_ptr(), 52) };
    // the clock "went backward": the file looks written in 1970
    let p = CString::new(flag_path(&c).to_str().unwrap()).unwrap();
    let t = [1i64, 0, 1, 0]; // atime/mtime = 1 s past the epoch (tv_sec, tv_usec)
    assert_eq!(unsafe { utimes(p.as_ptr(), &t) }, 0);
    let t0 = Instant::now();
    while !CLOCK_RAN.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        CLOCK_RAN.load(Ordering::SeqCst),
        "a flag with an old mtime still asks for the exit"
    );
}

/// Final review (console, outage 2026-10-03): a new instance starts its AVA1 side only once :9120 is
/// free. A port that stays answered ends the wait after the bound, not before and not much after.
#[test]
fn waiting_for_a_held_port_gives_up_after_the_bound() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = Instant::now();
    let rc = unsafe { takeover_wait_port_free(port as c_int, 400, 20) };
    let took = t.elapsed();
    assert_eq!(rc, -1, "the port is answered: the wait must say so");
    assert!(
        took >= Duration::from_millis(400),
        "gave up early: {took:?}"
    );
    assert!(
        took < Duration::from_millis(1500),
        "waited far past the bound: {took:?}"
    );
    drop(l);
}

#[test]
fn waiting_for_a_port_returns_the_moment_it_frees() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(l); // the old helper let go
    });
    let t = Instant::now();
    let rc = unsafe { takeover_wait_port_free(port as c_int, 5000, 20) };
    h.join().unwrap();
    assert_eq!(rc, 0);
    assert!(
        t.elapsed() < Duration::from_millis(2000),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn waiting_for_a_free_port_does_not_wait() {
    let port = free_port();
    let t = Instant::now();
    assert_eq!(
        unsafe { takeover_wait_port_free(port as c_int, 5000, 20) },
        0
    );
    assert!(t.elapsed() < Duration::from_millis(500));
}
