//! Lane socket buffers on the console side (review 003 section 1).
#![cfg(unix)]
use ava1_ctest as _; // links the payload C
use std::os::raw::c_int;

#[test]
fn a_lane_socket_asks_for_four_mib_each_way() {
    extern "C" {
        fn ava1_conn_tune_buffers(fd: c_int, rcv: *mut c_int, snd: *mut c_int) -> c_int;
    }
    use std::os::fd::AsRawFd;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (s, _) = l.accept().unwrap();
    let (mut rcv, mut snd) = (0, 0);
    let rc = unsafe { ava1_conn_tune_buffers(s.as_raw_fd(), &mut rcv, &mut snd) };
    assert_eq!(rc, 0);
    // The kernel may cap the request (Linux CI caps the send side at wmem_max, reported
    // doubled as 416 KiB; the PS5 gives 512 KiB), so assert only that tuning lifted both well
    // above a stock default (16-87 KiB), not the full 4 MiB ask.
    assert!(rcv >= 128 << 10, "rcv {rcv}");
    assert!(snd >= 128 << 10, "snd {snd}");
    drop(c);
}
