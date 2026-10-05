//! The payload's AVA1 C, built for the host (see build.rs). Test-only.
#![cfg(unix)]

pub mod mgmt_fs;
use ava1::frame::Header;
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

pub mod ffi {
    use super::*;

    #[repr(C)]
    pub struct CHeader {
        pub ty: u8,
        pub flags: u8,
        pub channel: u32,
        pub body_len: u32,
    }

    extern "C" {
        pub fn ava1_roundtrip(
            name: *const c_char,
            inp: *const u8,
            in_len: usize,
            out: *mut u8,
            cap: usize,
            out_len: *mut usize,
        ) -> c_int;
        pub fn ava1_utf8_valid(s: *const u8, n: usize) -> c_int;
        pub fn ava1_crc32c(p: *const u8, n: usize) -> u32;
        pub fn ava1_header_encode(h: *const CHeader, out: *mut u8);
        pub fn ava1_header_decode(inp: *const u8, h: *mut CHeader) -> c_int;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CIdentity {
        pub secret: [u8; 32],
        pub public: [u8; 32],
    }

    /// Mirrors ava1_noise_t (checked by `ava1_test_sizeof_noise`).
    #[repr(C)]
    pub struct CNoise {
        pub ck: [u8; 64],
        pub h: [u8; 64],
        pub k: [u8; 32],
        pub has_k: c_int,
        pub n: u64,
        pub s: CIdentity,
        pub e: CIdentity,
        pub rs: [u8; 32],
        pub re: [u8; 32],
        pub initiator: c_int,
        pub step: c_int,
        pub failed: c_int,
    }

    extern "C" {
        pub static mut ava1_trust_slot: [u8; 64];
        pub fn ava1_trust_slot_key(out: *mut u8) -> c_int;
        pub fn ava1_trust_slot_token(out: *mut u8) -> c_int;
        pub fn ava1_launch_proof(token: *const u8, h: *const u8, out: *mut u8);
    }

    extern "C" {
        pub fn ava1_test_sizeof_noise() -> usize;
        pub fn ava1_identity_from_secret(id: *mut CIdentity, secret: *const u8);
        pub fn ava1_lane_key(dir: *const u8, lane: u16, cn: *const u8, sn: *const u8, out: *mut u8);
        pub fn ava1_control_key(dir: *const u8, out: *mut u8);
        pub fn ava1_join_tag(
            dir: *const u8,
            sid: *const u8,
            lane: u16,
            cn: *const u8,
            out: *mut u8,
        );
        pub fn ava1_join_ack_tag(
            dir: *const u8,
            sid: *const u8,
            lane: u16,
            cn: *const u8,
            sn: *const u8,
            out: *mut u8,
        );
        /// Feeds `frame` to the C reader of a connection keyed with `key` (counter 0).
        /// 0 = opened; AVA1_E_* otherwise.
        pub fn ava1_test_conn_open_frame(key: *const u8, frame: *const u8, len: usize) -> c_int;
        /// Review 006 #1: counters in lockstep across frame kinds, and the nonce ceiling
        /// refuses to seal or open. 0 = ok, negative = which check failed.
        pub fn ava1_test_conn_nonce_ceiling(key: *const u8) -> c_int;
        /// Review 006 #4: frames the C reader opens from `wire` before its first error, and
        /// that error (AVA1_E_*).
        pub fn ava1_test_conn_read_all(
            key: *const u8,
            wire: *const u8,
            len: usize,
            opened: *mut u32,
        ) -> c_int;
        pub fn ava1_cpace_generator(h: *const u8, code: u32, g: *mut u8);
        pub fn ava1_cpace_public(x: *const u8, g: *const u8, y: *mut u8) -> c_int;
        pub fn ava1_cpace_key(
            h: *const u8,
            x: *const u8,
            y_peer: *const u8,
            ya: *const u8,
            yb: *const u8,
            k: *mut u8,
        ) -> c_int;
        pub fn ava1_cpace_mac(k: *const u8, server: c_int, h: *const u8, out: *mut u8);
        pub fn crypto_elligator_map(curve: *mut u8, hidden: *const u8);
        pub fn ava1_pair_commit(nonce_s: *const u8, out: *mut u8);
        pub fn ava1_noise_init(
            ns: *mut CNoise,
            initiator: c_int,
            s: *const CIdentity,
            e: *const CIdentity,
            prologue: *const u8,
            plen: usize,
        );
        pub fn ava1_noise_write(
            ns: *mut CNoise,
            payload: *const u8,
            plen: usize,
            out: *mut u8,
            cap: usize,
            out_len: *mut usize,
        ) -> c_int;
        pub fn ava1_noise_read(
            ns: *mut CNoise,
            msg: *const u8,
            len: usize,
            payload: *mut u8,
            cap: usize,
            plen: *mut usize,
        ) -> c_int;
        pub fn ava1_noise_split(ns: *const CNoise, k_i2r: *mut u8, k_r2i: *mut u8) -> c_int;
        pub fn ava1_seal(
            key: *const u8,
            n: u64,
            ad: *const u8,
            ad_len: usize,
            buf: *mut u8,
            len: usize,
            mac: *mut u8,
        );
        pub fn ava1_open(
            key: *const u8,
            n: u64,
            ad: *const u8,
            ad_len: usize,
            buf: *mut u8,
            len: usize,
            mac: *const u8,
        ) -> c_int;
        /// "avx2" or "portable" (NUL-terminated).
        pub fn ava1_aead_backend() -> *const c_char;
        /// 0 forces the portable ChaCha20, 1 restores CPUID selection.
        pub fn ava1_aead_allow_simd(allow: c_int);
    }

    extern "C" {
        pub fn ava1_b3_group_cv(data: *const u8, len: usize, index: u64, cv: *mut u8);
        pub fn ava1_b3_root_from_cvs(cvs: *const [u8; 32], n: u64, root: *mut u8);
        pub fn ava1_b3_hash(data: *const u8, len: usize, out: *mut u8);
    }

    #[repr(C)]
    pub struct CPeer {
        pub key: [u8; 32],
        pub added_unix: u64,
        pub name: [c_char; 64],
    }

    #[repr(C)]
    pub struct CPeers {
        pub p: [CPeer; 32],
        pub n: c_int,
    }

    /// Mirrors ava1_wtune_t (ava1_tune.c).
    #[repr(C)]
    #[derive(Default)]
    pub struct CTuneRaw {
        pub workers: u8,
        pub start: u8,
        pub min: u8,
        pub max: u8,
        pub before: f64,
        pub trying: c_int,
        pub hold: u32,
        pub idle: u32,
    }

    /// Mirrors ava1_test_opts_t in csrc/test_shim.c.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct TestOpts {
        pub pairing_s: u32,
        pub ping_ms: u32,
        pub dead_ms: u32,
        pub handshake_ms: u32,
        /// 0 = the server's default, here and below.
        pub min_frame_rate: u32,
        pub max_conns_per_ip: u32,
        pub max_unpaired: u32,
        pub pair_confirm_ms: u32,
        pub notify_every_ms: u32,
        pub max_pair_fails_per_ip: u32,
        pub max_pair_fails_total: u32,
        pub max_welcomes_per_ip: u32,
        pub notice_burst: u32,
        pub notice_refill_ms: u32,
        /// 1: the trust slot's key is `launch_key` and, with 2, its token `launch_token`
        /// (what ava1_glue.c passes the server from the slot).
        pub launch: u32,
        pub launch_key: [u8; 32],
        pub launch_token: [u8; 16],
    }

    extern "C" {
        pub fn ava1_test_server_start(
            secret: *const u8,
            peers_path: *const c_char,
            opts: *const TestOpts,
        ) -> c_int;
        /// The C server with the echo data hooks (test_shim.c).
        pub fn ava1_test_server_start_echo(
            secret: *const u8,
            peers_path: *const c_char,
            ping_ms: u32,
            dead_ms: u32,
            handshake_ms: u32,
        ) -> c_int;
        /// ava1_conn_post's bounded queue (test_shim.c): fill it while the writer
        /// cannot drain, read every frame back whole and in order, then check the
        /// bound refuses with AVA1_E_BUSY and breaks the connection. 0 = ok.
        pub fn ava1_test_post_queue(key: *const u8) -> c_int;
        pub fn ava1_test_sizeof_opts() -> usize;
        pub fn ava1_test_sizeof_pairlimit() -> usize;
        pub fn ava1_server_pair_guesses() -> u32;
        pub fn ava1_pl_init(p: *mut u8, per_ip: u32, total: u32, welcome: u32, win_ms: u64);
        pub fn ava1_pl_reset(p: *mut u8);
        pub fn ava1_pl_guess_allowed(p: *mut u8, ip: u32, now_ms: u64) -> c_int;
        pub fn ava1_pl_reserve(p: *mut u8, ip: u32, now_ms: u64, ip_fails: *mut u32) -> c_int;
        pub fn ava1_pl_release(p: *mut u8, ip: u32);
        pub fn ava1_pl_spent(p: *const u8) -> c_int;
        pub fn ava1_pl_welcome_allowed(p: *mut u8, ip: u32, now_ms: u64) -> c_int;
        pub fn ava1_test_pair_requests() -> u32;
        pub fn ava1_test_last_pair_code() -> u32;
        pub fn ava1_test_logs() -> u32;
        pub fn ava1_server_open_pairing(seconds: u32);
        pub fn ava1_server_pairing_open() -> c_int;
        pub fn ava1_server_stop();
        pub fn ava1_server_conns() -> c_int;
        pub fn ava1_identity_load_or_create(path: *const c_char, id: *mut CIdentity) -> c_int;
        pub fn ava1_peers_load(ps: *mut CPeers, path: *const c_char) -> c_int;
        pub fn ava1_peers_contains(ps: *const CPeers, key: *const u8) -> c_int;
        pub fn ava1_test_records_helpers(
            blob: *const u8,
            len: u32,
            out: *mut u8,
            cap: usize,
            out_len: *mut usize,
            count: *mut u32,
        ) -> c_int;
        pub fn ava1_test_rset_after(
            ops: *const u64,
            nops: usize,
            out: *mut u64,
            cap: usize,
        ) -> usize;
        pub fn ava1_test_journal_dump(dir: *const c_char, out: *mut u8, cap: usize) -> usize;
        pub fn ava1_test_journal_write_sample(dir: *const c_char) -> c_int;
        pub fn ava1_test_journal_compact(
            dir: *const c_char,
            open: *const u8,
            open_len: usize,
            snap: *const u8,
            snap_len: usize,
            done: *const u8,
            done_len: usize,
        ) -> c_int;
        pub fn ava1_test_bits_runs(
            n: u32,
            set: *const u32,
            nset: usize,
            out: *mut u32,
            cap: usize,
        ) -> usize;
        pub fn ava1_wtune_init(t: *mut CTuneRaw, start: u8, min: u8, max: u8);
        pub fn ava1_wtune_step(t: *mut CTuneRaw, files_per_s: f64, backlog: c_int) -> u8;
        pub fn ava1_test_mstore_pages(
            pages: *const *const u8,
            lens: *const usize,
            n: usize,
            hash: *mut u8,
            count: *mut u32,
            bytes: *mut u64,
        ) -> c_int;
        pub fn ava1_test_mstore_walk(root: *const c_char, hash: *mut u8, count: *mut u32) -> c_int;
        pub fn ava1_test_mstore_walk_ex(
            root: *const c_char,
            flags: u32,
            hash: *mut u8,
            count: *mut u32,
        ) -> c_int;
        pub fn ava1_test_thread_smoke(stack_bytes: *mut usize) -> c_int;
        pub fn ava1_test_ment_size() -> usize;
        pub fn ava1_test_path_ok(p: *const u8, n: usize) -> c_int;
        pub fn ava1_test_add_one(
            file_id: u32,
            kind: u8,
            size: u64,
            path: *const u8,
            plen: u16,
            existing: u64,
            out: *mut u64,
        );
        pub fn ava1_test_mstore_cap(out: *mut i64);
        pub fn ava1_test_mstore_roundtrip(
            pages: *const *const u8,
            lens: *const usize,
            n: usize,
            hash: *mut u8,
            hash2: *mut u8,
            blob: *mut u8,
            blob_cap: usize,
            blob_len: *mut usize,
            nroots: *mut u32,
            cpages: *mut u8,
            cpages_cap: usize,
            cpages_len: *mut usize,
        ) -> c_int;
        pub fn ava1_test_page_next(out: *mut i64);
        pub fn ava1_test_data_clamp(start: u8, min: u8, max: u8, out: *mut c_int);
        pub fn ava1_test_set_same_device(v: c_int);
        pub fn ava1_test_fsj_delay_us(us: u32);
        pub fn ava1_test_fsj_cross_name(name: *const c_char);
        pub fn ava1_test_copy_atomic(
            src: *const c_char,
            dst: *const c_char,
            blocks: c_int,
        ) -> c_int;
        pub fn ava1_test_reap_far();
        pub fn ava1_test_job_count() -> u32;
        pub fn ava1_test_mgmt_install() -> c_int;
        pub fn ava1_test_mgmt_install_duplicate() -> c_int;
        pub fn ava1_test_mgmt_uninstall();
        pub fn ava1_test_mgmt_status_for_token(t: *const c_char) -> c_int;
        pub fn ava1_test_mgmt_set_apps(n: u32);
        pub fn ava1_test_mgmt_stats(
            enters: *mut u32,
            leaves: *mut u32,
            last_frame: *mut u32,
            sony_peak: *mut c_int,
        );
        pub fn ava1_test_mgmt_last_path(out: *mut u8, cap: usize) -> usize;
        pub fn ava1_test_mgmt_install_diag() -> c_int;
        pub fn ava1_test_mgmt_diag_peak() -> c_int;
        pub fn ava1_test_mgmt_set_syslog(len: u32, mode: c_int);
        pub fn ava1_test_tail_window(
            text: *const u8,
            len: usize,
            cap: usize,
            start: *mut usize,
        ) -> c_int;
        pub fn ava1_test_events_set(path: *const c_char, limit: u32);
        pub fn ava1_test_events_log(line: *const c_char);
        pub fn ava1_test_set_allow_read(v: c_int);
        pub fn ava1_test_apply_begin(
            jobs: *const c_char,
            root: *const c_char,
            flags: u32,
            blob: *const u8,
            len: usize,
            fsync_delay_us: u32,
            crash_at: c_int,
        ) -> c_int;
        pub fn ava1_test_apply_chunk(id: u32, off: u64, d: *const u8, len: usize) -> c_int;
        pub fn ava1_test_apply_record(id: u32, d: *const u8, len: usize, root: *const u8) -> c_int;
        pub fn ava1_test_apply_root(id: u32, root: *const u8) -> c_int;
        pub fn ava1_test_apply_wait(timeout_ms: u32) -> c_int;
        pub fn ava1_test_apply_events(out: *mut u8, cap: usize) -> usize;
        pub fn ava1_test_apply_end();
        pub fn ava1_test_apply_bundle_raw(d: *const u8, len: usize, count: u32) -> c_int;
        pub fn ava1_test_apply_trace(on: c_int);
        pub fn ava1_test_apply_probe(out: *mut u64);
        pub fn ava1_test_apply_probe_prep(out: *mut u64);
        pub fn ava1_test_apply_opts2(v: *const u64);
        pub fn ava1_test_sweep_fail(n: c_int);
        pub fn ava1_test_sweep_fail_left() -> c_int;
        pub fn ava1_test_apply_unswept_bytes() -> u64;
        pub fn ava1_test_probe_open(byte: u8, root: *const c_char) -> c_int;
        pub fn ava1_test_house_ticks() -> u32;
        pub fn ava1_test_unswept_total() -> u64;
        pub fn ava1_test_unswept_global_add(d: i64);
        pub fn ava1_test_gc_boot(id: u64);
        pub fn ava1_test_exit_flush_create_fails(fail: c_int) -> c_int;
        pub fn ava1_test_jobs_gc(jobs: *const c_char, age_s: i64, max_age_s: i64) -> c_int;
        pub fn ava1_test_recv_restart_noopen() -> c_int;
        pub fn ava1_test_data_stop_only();
        pub fn ava1_test_reap_and_drop();
        pub fn ava1_test_apply_unswept() -> u32;
        pub fn ava1_test_apply_segments() -> u32;
        pub fn ava1_test_apply_hold_commit(on: c_int);
        pub fn ava1_test_set_log_small_flag(path: *const c_char);
        pub fn ava1_test_apply_fault_prealloc(id: u32);
        pub fn ava1_test_apply_compact() -> c_int;
        pub fn ava1_test_apply_commits_inflight() -> u32;
        pub fn ava1_test_apply_summary(out: *mut u8, cap: usize) -> usize;
        pub fn ava1_test_apply_timing() -> c_int;
        pub fn ava1_test_apply_hook_sleep(ms: u32);
        pub fn ava1_test_apply_dup_on_commit(id: u32, off: u64, d: *const u8, len: usize) -> c_int;
        pub fn ava1_test_apply_fail_dir_sync(id: u32);
        pub fn ava1_test_apply_hold(on: c_int);
        pub fn ava1_test_fsync_fault(point: c_int, n: c_int, err: c_int);
        pub fn ava1_test_fsync_retries() -> u32;
        pub fn ava1_test_fsync_calls() -> u32;
        pub fn ava1_test_fsync_pending_faults() -> c_int;
        pub fn ava1_test_apply_pending() -> u32;
        pub fn ava1_test_recv_open(
            jobs: *const c_char,
            root: *const c_char,
            flags: u32,
            policy: u8,
            entries: u32,
            crash_at: c_int,
        ) -> c_int;
        pub fn ava1_test_recv_staged() -> u8;
        pub fn ava1_test_recv_restart(crash_at: c_int) -> c_int;
        pub fn ava1_test_recv_page(page: *const u8, len: usize) -> c_int;
        pub fn ava1_test_recv_end(files: u32, bytes: u64, hash: *const u8) -> c_int;
        pub fn ava1_test_recv_resume(hash: *const u8) -> c_int;
        pub fn ava1_test_job_stopped() -> c_int;
        pub fn ava1_test_op_hold_reply_until_finished(on: c_int);
        pub fn ava1_test_recv_reopen(drop_old: c_int) -> c_int;
        pub fn ava1_test_recv_last_open() -> c_int;
        pub fn ava1_test_recv_ack_credit() -> u64;
        pub fn ava1_test_recv_set(root: *const c_char, owner: u8, deny_write: c_int);
        pub fn ava1_test_recv_args(kind: u8);
        pub fn ava1_test_apply_reserve(n: usize, take: c_int) -> c_int;
        #[allow(clippy::too_many_arguments)]
        pub fn ava1_test_server_start_data(
            secret: *const u8,
            peers_path: *const c_char,
            jobs_dir: *const c_char,
            ping_ms: u32,
            dead_ms: u32,
            handshake_ms: u32,
            fsync_delay_us: u32,
            port: u16,
            workers: u8,
        ) -> c_int;
        pub fn ava1_test_server_stop_data();
        pub fn ava1_test_payload_stop(conn_ms: c_int, sony_ms: c_int) -> c_int;
        pub fn ava1_test_intercept_shutdown(on: c_int);
        pub fn ava1_test_sony_lock();
        pub fn ava1_test_sony_unlock();
        pub fn ava1_test_data_delays(open_ms: u32, map_ms: u32);
        pub fn ava1_test_job_attached(id: *const u8) -> c_int;
        pub fn ava1_test_reap_rules(jobs_dir: *const c_char) -> c_int;
        pub fn ava1_test_retire_unlisted() -> c_int;
        pub fn ava1_test_job_counts(id: *const u8, out: *mut u64) -> c_int;
        pub fn ava1_test_data_knob(name: *const c_char, v: u32) -> c_int;
        pub fn ava1_test_fd_peak(which: c_int) -> u32;
        pub fn ava1_test_copy_walk_active() -> c_int;
        pub fn ava1_test_copy_delete_active() -> c_int;
        pub fn ava1_test_retiring_blocks_reopen() -> c_int;
        // P3 Task 7 (csrc/test_shim_t7.c)
        pub fn ava1_test_t7_install() -> c_int;
        pub fn ava1_test_t7_count() -> usize;
        pub fn ava1_test_t7_entry(
            i: usize,
            method: *mut u32,
            flags: *mut u32,
            frame: *mut u32,
            ack: *mut u32,
        ) -> c_int;
        pub fn ava1_test_t7_sony_peak() -> c_int;
        pub fn ava1_test_t7_reset_peak();
        pub fn ava1_test_copy_put_decode_failure() -> c_int;
        pub fn ava1_test_copy_retry_changed_message() -> c_int;
        pub fn ava1_test_send_chunk_bytes() -> u64;
        pub fn ava1_send_timing_enabled() -> c_int;
        pub fn ava1_send_test_begin(credit: u64);
        pub fn ava1_send_test_end();
        pub fn ava1_send_test_put(len: u64) -> c_int;
        pub fn ava1_send_test_take(lane: u16) -> u32;
        pub fn ava1_send_test_settle(seq: u32, rc: c_int) -> c_int;
        pub fn ava1_send_test_lane(lane: u16, up: c_int);
        pub fn ava1_send_test_received(seq: u32);
        pub fn ava1_send_test_credit(n: u64);
        pub fn ava1_send_test_stopping();
        pub fn ava1_send_test_state(out: *mut u64);
    }
}

/// The generated C per-struct records helpers (SPEC.md §3) over one blob of items: how
/// many items the blob holds, and the blob C rebuilds by re-appending each of them.
pub fn c_records_helpers(blob: &[u8]) -> Result<(Vec<u8>, u32), i32> {
    let mut out = vec![0u8; blob.len() + 64];
    let mut out_len = 0usize;
    let mut count = 0u32;
    let rc = unsafe {
        ffi::ava1_test_records_helpers(
            blob.as_ptr(),
            blob.len() as u32,
            out.as_mut_ptr(),
            out.len(),
            &mut out_len,
            &mut count,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    out.truncate(out_len);
    Ok((out, count))
}

pub fn c_roundtrip(name: &str, input: &[u8]) -> Result<Vec<u8>, i32> {
    let n = CString::new(name).unwrap();
    let mut out = vec![0u8; input.len() + 64];
    let mut len = 0usize;
    let rc = unsafe {
        ffi::ava1_roundtrip(
            n.as_ptr(),
            input.as_ptr(),
            input.len(),
            out.as_mut_ptr(),
            out.len(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    out.truncate(len);
    Ok(out)
}

pub fn c_crc32c(b: &[u8]) -> u32 {
    unsafe { ffi::ava1_crc32c(b.as_ptr(), b.len()) }
}

pub fn c_header_encode(h: Header) -> [u8; 16] {
    let ch = ffi::CHeader {
        ty: h.ty,
        flags: h.flags,
        channel: h.channel,
        body_len: h.body_len,
    };
    let mut out = [0u8; 16];
    unsafe { ffi::ava1_header_encode(&ch, out.as_mut_ptr()) };
    out
}

pub fn c_header_decode(b: &[u8; 16]) -> Result<Header, i32> {
    let mut ch = ffi::CHeader {
        ty: 0,
        flags: 0,
        channel: 0,
        body_len: 0,
    };
    match unsafe { ffi::ava1_header_decode(b.as_ptr(), &mut ch) } {
        0 => Ok(Header {
            ty: ch.ty,
            flags: ch.flags,
            channel: ch.channel,
            body_len: ch.body_len,
        }),
        e => Err(e),
    }
}

pub fn c_utf8_valid(b: &[u8]) -> bool {
    unsafe { ffi::ava1_utf8_valid(b.as_ptr(), b.len()) != 0 }
}

pub fn c_b3_group_cv(d: &[u8], index: u64) -> [u8; 32] {
    let mut cv = [0u8; 32];
    unsafe { ffi::ava1_b3_group_cv(d.as_ptr(), d.len(), index, cv.as_mut_ptr()) };
    cv
}

pub fn c_b3_root(cvs: &[[u8; 32]]) -> [u8; 32] {
    let mut r = [0u8; 32];
    unsafe { ffi::ava1_b3_root_from_cvs(cvs.as_ptr(), cvs.len() as u64, r.as_mut_ptr()) };
    r
}

pub fn c_b3_hash(d: &[u8]) -> [u8; 32] {
    let mut r = [0u8; 32];
    unsafe { ffi::ava1_b3_hash(d.as_ptr(), d.len(), r.as_mut_ptr()) };
    r
}

pub fn c_identity(secret: [u8; 32]) -> ffi::CIdentity {
    let mut id = ffi::CIdentity {
        secret: [0; 32],
        public: [0; 32],
    };
    unsafe { ffi::ava1_identity_from_secret(&mut id, secret.as_ptr()) };
    id
}

/// One side of a C Noise handshake.
pub struct CHandshake(Box<ffi::CNoise>);

impl CHandshake {
    pub fn new(initiator: bool, s: [u8; 32], e: [u8; 32], prologue: &[u8]) -> Self {
        let (s, e) = (c_identity(s), c_identity(e));
        let mut ns: Box<ffi::CNoise> = Box::new(unsafe { std::mem::zeroed() });
        unsafe {
            ffi::ava1_noise_init(
                &mut *ns,
                initiator as c_int,
                &s,
                &e,
                prologue.as_ptr(),
                prologue.len(),
            )
        };
        CHandshake(ns)
    }
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, i32> {
        let mut out = vec![0u8; payload.len() + 128];
        let mut n = 0usize;
        match unsafe {
            ffi::ava1_noise_write(
                &mut *self.0,
                payload.as_ptr(),
                payload.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut n,
            )
        } {
            0 => {
                out.truncate(n);
                Ok(out)
            }
            e => Err(e),
        }
    }
    pub fn read(&mut self, msg: &[u8]) -> Result<Vec<u8>, i32> {
        let mut out = vec![0u8; msg.len()];
        let mut n = 0usize;
        match unsafe {
            ffi::ava1_noise_read(
                &mut *self.0,
                msg.as_ptr(),
                msg.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut n,
            )
        } {
            0 => {
                out.truncate(n);
                Ok(out)
            }
            e => Err(e),
        }
    }
    pub fn hash(&self) -> [u8; 64] {
        self.0.h
    }
    pub fn remote_static(&self) -> [u8; 32] {
        self.0.rs
    }
    /// Panics unless the handshake completed.
    pub fn split(&self) -> ([u8; 32], [u8; 32]) {
        self.try_split().expect("handshake complete")
    }

    pub fn try_split(&self) -> Result<([u8; 32], [u8; 32]), i32> {
        let (mut a, mut b) = ([0xffu8; 32], [0xffu8; 32]);
        match unsafe { ffi::ava1_noise_split(&*self.0, a.as_mut_ptr(), b.as_mut_ptr()) } {
            0 => Ok((a, b)),
            e => {
                assert_eq!((a, b), ([0u8; 32], [0u8; 32]), "no key material on failure");
                Err(e)
            }
        }
    }

    /// Replaces the static public key this side will send (to play a hostile peer).
    pub fn set_static_public(&mut self, p: [u8; 32]) {
        self.0.s.public = p;
    }
}

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

static C_SERVER: Mutex<()> = Mutex::new(());

/// The payload's server, running on 127.0.0.1. One at a time per process.
pub struct CServer {
    pub port: u16,
    data: Option<DataArgs>,
    _lock: MutexGuard<'static, ()>,
}

/// What `restart_data` needs to start the same data server again.
struct DataArgs {
    secret: [u8; 32],
    peers: CString,
    jobs: CString,
    times: [u32; 4],
    workers: u8,
}

fn start_data_raw(a: &DataArgs, port: u16) -> u16 {
    let rc = unsafe {
        ffi::ava1_test_server_start_data(
            a.secret.as_ptr(),
            a.peers.as_ptr(),
            a.jobs.as_ptr(),
            a.times[0],
            a.times[1],
            a.times[2],
            a.times[3],
            port,
            a.workers,
        )
    };
    assert!(rc > 0, "C data server failed to start: {rc}");
    rc as u16
}

/// Serialises the tests that install the shim's process-wide tables, policies and counters
/// (test_shim.c / test_shim_fs.c): hold the guard for the whole test.
static SHIM_LOCK: Mutex<()> = Mutex::new(());

impl CServer {
    /// The shared guard for shim-global state; take it before installing anything.
    pub fn lock_for_shim_tests() -> MutexGuard<'static, ()> {
        SHIM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn start(
        secret: [u8; 32],
        peers_path: &Path,
        pairing_s: u32,
        ping_ms: u32,
        dead_ms: u32,
        handshake_ms: u32,
    ) -> Self {
        Self::start_with(
            secret,
            peers_path,
            ffi::TestOpts {
                pairing_s,
                ping_ms,
                dead_ms,
                handshake_ms,
                ..Default::default()
            },
        )
    }

    pub fn start_with(secret: [u8; 32], peers_path: &Path, opts: ffi::TestOpts) -> Self {
        let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        let p = CString::new(peers_path.to_str().unwrap()).unwrap();
        let rc = unsafe { ffi::ava1_test_server_start(secret.as_ptr(), p.as_ptr(), &opts) };
        assert!(rc > 0, "C server failed to start: {rc}");
        CServer {
            port: rc as u16,
            data: None,
            _lock: lock,
        }
    }

    /// The C server with the echo data hooks (test_shim.c).
    pub fn start_echo(
        secret: [u8; 32],
        peers_path: &Path,
        ping_ms: u32,
        dead_ms: u32,
        hs_ms: u32,
    ) -> Self {
        let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        let p = CString::new(peers_path.to_str().unwrap()).unwrap();
        let rc = unsafe {
            ffi::ava1_test_server_start_echo(secret.as_ptr(), p.as_ptr(), ping_ms, dead_ms, hs_ms)
        };
        assert!(rc > 0, "C server failed to start: {rc}");
        CServer {
            port: rc as u16,
            data: None,
            _lock: lock,
        }
    }

    /// The C server with the real data layer (Task 14): uploads land under any absolute
    /// path. `fsync_delay_us` slows every data fsync (a slow disk).
    pub fn start_data(
        secret: [u8; 32],
        peers: &Path,
        jobs: &Path,
        ping_ms: u32,
        dead_ms: u32,
        hs_ms: u32,
        fsync_delay_us: u32,
    ) -> Self {
        Self::start_data_with(
            secret,
            peers,
            jobs,
            ping_ms,
            dead_ms,
            hs_ms,
            fsync_delay_us,
            0,
        )
    }

    /// `start_data` with a fixed number of apply workers (0 = the defaults).
    #[allow(clippy::too_many_arguments)]
    pub fn start_data_with(
        secret: [u8; 32],
        peers: &Path,
        jobs: &Path,
        ping_ms: u32,
        dead_ms: u32,
        hs_ms: u32,
        fsync_delay_us: u32,
        workers: u8,
    ) -> Self {
        Self::start_data_opts(
            secret,
            peers,
            jobs,
            ping_ms,
            dead_ms,
            hs_ms,
            fsync_delay_us,
            workers,
            LogOpts::default(),
        )
    }

    /// `start_data_with` and the durable-by-log options of the data layer (see `LogOpts`).
    #[allow(clippy::too_many_arguments)]
    pub fn start_data_opts(
        secret: [u8; 32],
        peers: &Path,
        jobs: &Path,
        ping_ms: u32,
        dead_ms: u32,
        hs_ms: u32,
        fsync_delay_us: u32,
        workers: u8,
        opts: LogOpts,
    ) -> Self {
        let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        opts.apply();
        let a = DataArgs {
            secret,
            peers: CString::new(peers.to_str().unwrap()).unwrap(),
            jobs: CString::new(jobs.to_str().unwrap()).unwrap(),
            times: [ping_ms, dead_ms, hs_ms, fsync_delay_us],
            workers,
        };
        let port = start_data_raw(&a, 0);
        CServer {
            port,
            data: Some(a),
            _lock: lock,
        }
    }

    /// The two halves of `restart_data`: stop the server and data layer (every job freed,
    /// the disk kept), or start them again on the port the first start used.
    pub fn stop_data_only(&mut self) {
        self.data.as_ref().expect("stop_data_only needs start_data");
        unsafe { ffi::ava1_test_server_stop_data() };
    }

    /// The payload's real exit sequence (`ava1_payload_stop`): returns its result bits.
    pub fn payload_stop(&mut self, conn_ms: i32, sony_ms: i32) -> i32 {
        self.data.as_ref().expect("payload_stop needs start_data");
        unsafe { ffi::ava1_test_payload_stop(conn_ms, sony_ms) }
    }

    /// Answer `node.shutdown` with the payload's real shape (reply, then `ava1_payload_stop` 300 ms
    /// later) instead of the installed management table. Process-wide; off by default.
    pub fn intercept_shutdown(&self, on: bool) {
        unsafe { ffi::ava1_test_intercept_shutdown(c_int::from(on)) }
    }

    /// Holds / releases the Sony API lock, as a handler in the middle of a Sony call does.
    pub fn sony_lock(&self, held: bool) {
        unsafe {
            if held {
                ffi::ava1_test_sony_lock()
            } else {
                ffi::ava1_test_sony_unlock()
            }
        }
    }

    pub fn start_data_again(&mut self) {
        let a = self
            .data
            .as_ref()
            .expect("start_data_again needs start_data");
        self.port = start_data_raw(a, self.port);
    }

    /// A payload restart: server and data layer stopped (every job freed, the disk kept)
    /// and started again on the same port.
    pub fn restart_data(&mut self) {
        self.stop_data_only();
        self.start_data_again();
    }

    /// Test hooks: JobOpen's work waits `open_ms` before it starts; an OK map waits
    /// `map_ms` before it is sent. 0 = off. Reset by every start.
    pub fn set_open_delay_ms(&self, ms: u32) {
        unsafe { ffi::ava1_test_data_delays(ms, u32::MAX) }
    }

    pub fn set_map_delay_ms(&self, ms: u32) {
        unsafe { ffi::ava1_test_data_delays(u32::MAX, ms) }
    }

    /// Fail the directory sync at the named file, or the staged tree (u32::MAX).
    pub fn fail_dir_sync(&self, id: u32) {
        unsafe { ffi::ava1_test_apply_fail_dir_sync(id) }
    }

    /// (bytes the apply engine has received, lane frames held) for job `id`.
    pub fn job_counts(&self, id: [u8; 16]) -> (u64, u64) {
        let mut out = [0u64; 2];
        assert_eq!(
            unsafe { ffi::ava1_test_job_counts(id.as_ptr(), out.as_mut_ptr()) },
            0
        );
        (out[0], out[1])
    }

    /// A data-layer test knob (test_shim.c `ava1_test_data_knob`): ack_fail (the ack's send
    /// "fails" with -v), feeder_fail, feed_delay_ms, park_ms, ctl_cap, reserve_fail,
    /// lane_alloc_fail, fb_force (every Received takes the waiting-send fallback); the
    /// download sender's send_fail / writer_start_fail (the next v lane sends / writer
    /// starts fail) and chunk_bytes (sets the queued-Chunk-bytes counter).
    pub fn knob(&self, name: &str, v: u32) {
        let n = CString::new(name).unwrap();
        assert_eq!(
            unsafe { ffi::ava1_test_data_knob(n.as_ptr(), v) },
            0,
            "{name}"
        );
    }

    /// High-water mark of open pending small-file fds (0) or fds held by disk.calibrate (1)
    /// since the `fd_peak_reset` knob.
    pub fn fd_peak(&self, which: i32) -> u32 {
        unsafe { ffi::ava1_test_fd_peak(which) }
    }

    pub fn copy_walk_active(&self) -> bool {
        unsafe { ffi::ava1_test_copy_walk_active() != 0 }
    }

    pub fn copy_delete_active(&self) -> bool {
        unsafe { ffi::ava1_test_copy_delete_active() != 0 }
    }

    /// Chunk bytes the C download sender has queued since knob "chunk_bytes" set it.
    pub fn sent_chunk_bytes(&self) -> u64 {
        unsafe { ffi::ava1_test_send_chunk_bytes() }
    }

    /// 1 attached to a session, 0 parked, -1 not in the job table.
    pub fn job_attached(&self, id: [u8; 16]) -> i32 {
        unsafe { ffi::ava1_test_job_attached(id.as_ptr()) }
    }

    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub fn conns(&self) -> i32 {
        unsafe { ffi::ava1_server_conns() }
    }

    /// Lines the server has logged since it started.
    pub fn logs(&self) -> u32 {
        unsafe { ffi::ava1_test_logs() }
    }

    pub fn open_pairing(&self, seconds: u32) {
        unsafe { ffi::ava1_server_open_pairing(seconds) }
    }

    pub fn pairing_open(&self) -> bool {
        unsafe { ffi::ava1_server_pairing_open() != 0 }
    }

    /// Wrong guesses at the code since the window was last opened.
    pub fn pair_guesses(&self) -> u32 {
        unsafe { ffi::ava1_server_pair_guesses() }
    }

    pub fn pair_requests(&self) -> (u32, u32) {
        unsafe {
            (
                ffi::ava1_test_pair_requests(),
                ffi::ava1_test_last_pair_code(),
            )
        }
    }
}

impl CServer {
    /// While set, the data server's preallocation of file `id` answers ENOSPC (`None` clears it).
    /// Process-wide like the rest of the data layer's test hooks; hold the server while it is set.
    pub fn fault_prealloc(&self, id: Option<u32>) {
        unsafe { ffi::ava1_test_apply_fault_prealloc(id.unwrap_or(u32::MAX - 1)) }
    }
}

impl Drop for CServer {
    fn drop(&mut self) {
        if self.data.is_some() {
            unsafe { ffi::ava1_test_server_stop_data() };
        } else {
            unsafe { ffi::ava1_server_stop() };
        }
    }
}

/// `ava1_job_retire` on a job that is no longer listed (0 = refused, as it must be).
pub fn c_retire_unlisted() -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { ffi::ava1_test_retire_unlisted() }
}

/// A reaped job still held keeps its id until destroyed (0 = holds).
pub fn c_retiring_blocks_reopen() -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { ffi::ava1_test_retiring_blocks_reopen() }
}

/// A copy's in-process put frees a malformed Chunk/Bundle and returns its reserved bytes
/// (0 = it does, else the failing step).
pub fn c_copy_put_decode_failure() -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { ffi::ava1_test_copy_put_decode_failure() }
}

/// A changed-file retry reaches a copy job as ERR_VERIFY with the "source changed while
/// copying" message (0 = it does, else the failing step).
pub fn c_copy_retry_changed_message() -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { ffi::ava1_test_copy_retry_changed_message() }
}

/// The reaper's rules, checked in C on a private data layer (0 = all hold, else the step
/// that failed). Holds the C server lock.
pub fn c_reap_rules(jobs: &Path) -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let j = CString::new(jobs.to_str().unwrap()).unwrap();
    unsafe { ffi::ava1_test_reap_rules(j.as_ptr()) }
}

extern "C" {
    fn ava1_test_firmware(kernel_version: *const c_char, out: *mut c_char, cap: usize);
}

/// The payload's node.info firmware string for kernel build string `kv`, written into a
/// `cap`-byte buffer (payload/include/ps5_firmware.h).
pub fn c_firmware_from_kernel(kv: &str, cap: usize) -> String {
    let kv = CString::new(kv).unwrap();
    let mut out = vec![0x55 as c_char; cap];
    unsafe { ava1_test_firmware(kv.as_ptr(), out.as_mut_ptr(), cap) };
    unsafe { std::ffi::CStr::from_ptr(out.as_ptr()) }
        .to_str()
        .unwrap()
        .to_string()
}

pub fn c_identity_load_or_create(path: &Path) -> Result<[u8; 32], i32> {
    let p = CString::new(path.to_str().unwrap()).unwrap();
    let mut id = ffi::CIdentity {
        secret: [0; 32],
        public: [0; 32],
    };
    match unsafe { ffi::ava1_identity_load_or_create(p.as_ptr(), &mut id) } {
        0 => Ok(id.public),
        e => Err(e),
    }
}

/// (number of peers loaded, whether `key` is among them)
pub fn c_peers_load(path: &Path, key: &[u8; 32]) -> (i32, bool) {
    let p = CString::new(path.to_str().unwrap()).unwrap();
    let mut ps: Box<std::mem::MaybeUninit<ffi::CPeers>> = Box::new(std::mem::MaybeUninit::uninit());
    unsafe {
        assert_eq!(ffi::ava1_peers_load(ps.as_mut_ptr(), p.as_ptr()), 0);
        let ps = ps.assume_init_ref();
        (ps.n, ffi::ava1_peers_contains(ps, key.as_ptr()) != 0)
    }
}

/// ava1_conn_post's bounded queue (test_shim.c): fills it while the writer cannot
/// drain, reads every frame back whole and in order, then checks the bound refuses
/// with AVA1_E_BUSY and breaks the connection. 0 = ok, negative = which check failed.
pub fn c_post_queue(key: [u8; 32]) -> i32 {
    unsafe { ffi::ava1_test_post_queue(key.as_ptr()) }
}

pub fn c_rset_after(ops: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let flat: Vec<u64> = ops.iter().flat_map(|(s, e)| [*s, *e]).collect();
    let mut out = vec![0u64; 2 * ops.len() + 2];
    let n = unsafe {
        ffi::ava1_test_rset_after(flat.as_ptr(), ops.len(), out.as_mut_ptr(), ops.len() + 1)
    };
    out.chunks(2).take(n).map(|p| (p[0], p[1])).collect()
}

pub fn c_bits_runs(n: u32, set: &[u32]) -> Vec<(u32, u32)> {
    let mut out = vec![0u32; 2 * (n as usize + 1)];
    let k = unsafe {
        ffi::ava1_test_bits_runs(n, set.as_ptr(), set.len(), out.as_mut_ptr(), n as usize + 1)
    };
    out.chunks(2).take(k).map(|p| (p[0], p[1])).collect()
}

/// The receiver's worker tuner (ava1_tune.c). Pure; step it once per 2 s tick.
pub struct CTune(ffi::CTuneRaw);

impl CTune {
    pub fn new(start: u8, min: u8, max: u8) -> Self {
        let mut t = ffi::CTuneRaw::default();
        unsafe { ffi::ava1_wtune_init(&mut t, start, min, max) };
        CTune(t)
    }

    pub fn step(&mut self, rate: f64, backlog: bool) -> u8 {
        unsafe { ffi::ava1_wtune_step(&mut self.0, rate, backlog as c_int) }
    }
}

/// Rebuilds the C manifest store from the pages the Rust side encoded and reports
/// the C store's hash, entry count and total bytes.
pub fn c_mstore_from_pages(pages: &[Vec<u8>]) -> (i32, [u8; 32], u32, u64) {
    let ptrs: Vec<*const u8> = pages.iter().map(|p| p.as_ptr()).collect();
    let lens: Vec<usize> = pages.iter().map(|p| p.len()).collect();
    let (mut h, mut n, mut b) = ([0u8; 32], 0u32, 0u64);
    let rc = unsafe {
        ffi::ava1_test_mstore_pages(
            ptrs.as_ptr(),
            lens.as_ptr(),
            pages.len(),
            h.as_mut_ptr(),
            &mut n,
            &mut b,
        )
    };
    (rc, h, n, b)
}

/// Walks `root` with the C store and reports its hash and entry count.
pub fn c_mstore_walk(root: &Path) -> (i32, [u8; 32], u32) {
    let r = CString::new(root.to_str().unwrap()).unwrap();
    let (mut h, mut n) = ([0u8; 32], 0u32);
    let rc = unsafe { ffi::ava1_test_mstore_walk(r.as_ptr(), h.as_mut_ptr(), &mut n) };
    (rc, h, n)
}

/// `c_mstore_walk` with flags: `1` is AVA1_WALK_FOLLOW (the download sender's mode).
pub fn c_mstore_walk_ex(root: &Path, flags: u32) -> (i32, [u8; 32], u32) {
    let r = CString::new(root.to_str().unwrap()).unwrap();
    let (mut h, mut n) = ([0u8; 32], 0u32);
    let rc = unsafe { ffi::ava1_test_mstore_walk_ex(r.as_ptr(), flags, h.as_mut_ptr(), &mut n) };
    (rc, h, n)
}

/// Starts a thread through ava1_thread_start; (rc, observed stack size in bytes).
/// rc is 0 only when the 200 KiB frame survived and the observed stack is within
/// [200 KiB, AVA1_THREAD_STACK + 4 KiB].
pub fn c_thread_smoke() -> (i32, usize) {
    let mut sz = 0usize;
    let rc = unsafe { ffi::ava1_test_thread_smoke(&mut sz) };
    (rc, sz)
}

/// The C replay of the journal at `dir`, as the text `c_style_dump` builds for the
/// same state.
pub fn c_journal_dump(dir: &Path) -> String {
    let d = CString::new(dir.to_str().unwrap()).unwrap();
    let mut out = vec![0u8; 1 << 20];
    let n = unsafe { ffi::ava1_test_journal_dump(d.as_ptr(), out.as_mut_ptr(), out.len()) };
    String::from_utf8(out[..n].to_vec()).unwrap()
}

/// Writes a sample journal from C; 0 or a negative error.
pub fn c_journal_write_sample(dir: &Path) -> i32 {
    let d = CString::new(dir.to_str().unwrap()).unwrap();
    unsafe { ffi::ava1_test_journal_write_sample(d.as_ptr()) }
}

/// Compacts the journal at `dir` with the C writer; `done` is None for an unfinished
/// job. 0 or a negative error.
pub fn c_journal_compact(dir: &Path, open: &[u8], snap: &[u8], done: Option<&[u8]>) -> i32 {
    let d = CString::new(dir.to_str().unwrap()).unwrap();
    let (dp, dn) = done.map_or((std::ptr::null(), 0), |b| (b.as_ptr(), b.len()));
    unsafe {
        ffi::ava1_test_journal_compact(
            d.as_ptr(),
            open.as_ptr(),
            open.len(),
            snap.as_ptr(),
            snap.len(),
            dp,
            dn,
        )
    }
}

/// sizeof(ava1_ment_t).
pub fn c_ment_size() -> usize {
    unsafe { ffi::ava1_test_ment_size() }
}

/// ava1_path_ok on raw bytes.
pub fn c_path_ok(p: &[u8]) -> bool {
    unsafe { ffi::ava1_test_path_ok(p.as_ptr(), p.len()) != 0 }
}

/// Result of one `ava1_mstore_add` onto a store that already holds one file "pre".
pub struct CAdd {
    pub rc: i32,
    pub stored_size: u64,
    pub bytes: u64,
    pub path_bounds_ok: bool,
}

pub fn c_add_one(file_id: u32, kind: u8, size: u64, path: &[u8], existing: u64) -> CAdd {
    let mut o = [0u64; 4];
    unsafe {
        ffi::ava1_test_add_one(
            file_id,
            kind,
            size,
            path.as_ptr(),
            path.len() as u16,
            existing,
            o.as_mut_ptr(),
        )
    };
    CAdd {
        rc: o[0] as i64 as i32,
        stored_size: o[1],
        bytes: o[2],
        path_bounds_ok: o[3] == 1,
    }
}

/// (reserve past the cap, add past the cap, capacity after reserve(223000)).
pub fn c_mstore_cap() -> (i64, i64, i64) {
    let mut o = [0i64; 3];
    unsafe { ffi::ava1_test_mstore_cap(o.as_mut_ptr()) };
    (o[0], o[1], o[2])
}

pub struct CRoundtrip {
    pub rc: i32,
    pub hash: [u8; 32],
    pub hash2: [u8; 32],
    pub blob: Vec<u8>,
    pub nroots: u32,
    /// Pages the C store encoded (job id [5; 16]).
    pub pages: Vec<Vec<u8>>,
}

/// pages -> C store -> blob -> second C store -> blob; plus C-encoded pages.
pub fn c_mstore_roundtrip(pages: &[Vec<u8>]) -> CRoundtrip {
    let ptrs: Vec<*const u8> = pages.iter().map(|p| p.as_ptr()).collect();
    let lens: Vec<usize> = pages.iter().map(|p| p.len()).collect();
    let (mut h, mut h2) = ([0u8; 32], [0u8; 32]);
    let mut blob = vec![0u8; 8 << 20];
    let mut cp = vec![0u8; 8 << 20];
    let (mut bl, mut nr, mut cl) = (0usize, 0u32, 0usize);
    let rc = unsafe {
        ffi::ava1_test_mstore_roundtrip(
            ptrs.as_ptr(),
            lens.as_ptr(),
            pages.len(),
            h.as_mut_ptr(),
            h2.as_mut_ptr(),
            blob.as_mut_ptr(),
            blob.len(),
            &mut bl,
            &mut nr,
            cp.as_mut_ptr(),
            cp.len(),
            &mut cl,
        )
    };
    blob.truncate(bl);
    let mut out = Vec::new();
    let mut at = 0;
    while at < cl {
        let n = u32::from_le_bytes(cp[at..at + 4].try_into().unwrap()) as usize;
        out.push(cp[at + 4..at + 4 + n].to_vec());
        at += 4 + n;
    }
    CRoundtrip {
        rc,
        hash: h,
        hash2: h2,
        blob,
        nroots: nr,
        pages: out,
    }
}

/// (rc small, next after small, rc big, next after big) of ava1_mstore_page.
pub fn c_page_next() -> (i64, i64, i64, i64) {
    let mut o = [0i64; 4];
    unsafe { ffi::ava1_test_page_next(o.as_mut_ptr()) };
    (o[0], o[1], o[2], o[3])
}

/// Effective (start, min, max) after ava1_data_start, the second start's rc, the first's.
pub fn c_data_clamp(start: u8, min: u8, max: u8) -> ([i32; 3], i32, i32) {
    let mut o = [0i32; 5];
    unsafe { ffi::ava1_test_data_clamp(start, min, max, o.as_mut_ptr()) };
    ([o[0], o[1], o[2]], o[3], o[4])
}

thread_local! {
    /// The durable-by-log off-switch file the next job begun or opened on this thread checks.
    static LOG_FLAG: std::cell::RefCell<Option<CString>> = const { std::cell::RefCell::new(None) };
}

/// Points the console's runtime durable-by-log off-switch (review 007 #5) at `path` for jobs this
/// thread begins or opens; the file's existence is what turns the logging off. None: the real path.
pub fn c_set_log_small_flag(path: Option<&Path>) {
    LOG_FLAG.with(|f| *f.borrow_mut() = path.map(|p| CString::new(p.to_str().unwrap()).unwrap()));
}

thread_local! {
    /// The same_device answer the next `CApplyJob::begin` on this thread installs.
    static SAME_DEVICE: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}

/// A folder or file with this base name reports another device than its parent (a mount point)
/// to the walkers' device guard; "" turns it off (every data start does).
pub fn c_set_cross_name(name: &str) {
    let c = std::ffi::CString::new(name).unwrap();
    unsafe { ffi::ava1_test_fsj_cross_name(c.as_ptr()) }
}

/// `fsj_copy_atomic(src, dst)` with a cancel after `blocks` 64 KiB blocks (negative: never):
/// 0, -1 or -2 (cancelled).
pub fn c_copy_atomic(src: &std::path::Path, dst: &std::path::Path, blocks: i32) -> i32 {
    let s = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
    let d = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
    unsafe { ffi::ava1_test_copy_atomic(s.as_ptr(), d.as_ptr(), blocks) }
}

/// Runs the payload's reaper as if an hour had passed: finished jobs are collected, running
/// ones stay.
/// Jobs in the C job table now.
pub fn c_job_count() -> u32 {
    unsafe { ffi::ava1_test_job_count() }
}

/// Bytes of done-but-unswept small files across all jobs (the cross-job counter).
pub fn c_unswept_global() -> u64 {
    unsafe { ffi::ava1_test_unswept_total() }
}

pub fn c_reap_far() {
    unsafe { ffi::ava1_test_reap_far() }
}

/// Every file a job.run operation visits waits this long (0 = off; reset at each data start).
pub fn c_set_fsj_delay_us(us: u32) {
    unsafe { ffi::ava1_test_fsj_delay_us(us) }
}

/// What the data layer's same_device hook answers for the next apply job begun on this
/// thread (1 same, 0 crosses, -1 unknown). It is installed under the C server lock by
/// `CApplyJob::begin` and cleared when that job ends, so it cannot reach a job another
/// test is running.
pub fn c_set_same_device(v: i32) {
    SAME_DEVICE.with(|c| c.set(v));
}

/// What the C data layer's `may_read` hook answers for the next download JobOpen
/// (test_shim.c's `t_allow_read`): false refuses the open with AVA1_ERR_PATH. The
/// refusal test restores it; a test that starts a server should set it true first.
/// Names the trust-store directory the data layer must never follow a link into (None: off).
pub fn c_set_protected(dir: Option<&std::path::Path>) {
    extern "C" {
        fn ava1_test_set_protected(dir: *const c_char);
    }
    match dir {
        Some(d) => {
            let c = CString::new(d.to_str().unwrap()).unwrap();
            unsafe { ava1_test_set_protected(c.as_ptr()) }
        }
        None => unsafe { ava1_test_set_protected(std::ptr::null()) },
    }
}

pub fn c_set_read_allowed(v: bool) {
    unsafe { ffi::ava1_test_set_allow_read(v as c_int) };
}

static C_SEND_WINDOW: Mutex<()> = Mutex::new(());

/// The C download sender's window and queues (ava1_send.c), driven step by step with no
/// threads and no network: the reader's put, a lane writer's pick and the settle of its
/// send, lane events, Received and Credit. One at a time per process.
pub struct CSendWindow {
    _lock: MutexGuard<'static, ()>,
}

/// What the C sender's window looks like right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSendState {
    pub credit: u64,
    pub ready: u64,
    pub queued: u64,
    pub inflight: u64,
}

/// `settle`'s answers: the writer goes on, its lane generation is over, the job is over.
pub const SETTLE_GO_ON: i32 = 1;
pub const SETTLE_EXIT: i32 = 0;
pub const SETTLE_FATAL: i32 = -1;
/// AVA1_E_IO / AVA1_E_CLOSED / AVA1_E_TOOLONG (ava1_wire.h), what a lane send returns.
pub const C_E_IO: i32 = -9;
pub const C_E_CLOSED: i32 = -12;
pub const C_E_TOOLONG: i32 = -8;

impl CSendWindow {
    pub fn new(credit: u64) -> Self {
        let lock = C_SEND_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { ffi::ava1_send_test_begin(credit) };
        CSendWindow { _lock: lock }
    }
    /// The reader queues one frame of `len` bytes: put_frame's return (0, or -ECANCELED).
    pub fn put(&self, len: u64) -> i32 {
        unsafe { ffi::ava1_send_test_put(len) }
    }
    /// The lane's writer picks a frame: its seq, or None when it takes nothing.
    pub fn take(&self, lane: u16) -> Option<u32> {
        match unsafe { ffi::ava1_send_test_take(lane) } {
            0 => None,
            s => Some(s),
        }
    }
    /// The writer's send of `seq` returned `rc`: one of the SETTLE_* answers.
    pub fn settle(&self, seq: u32, rc: i32) -> i32 {
        unsafe { ffi::ava1_send_test_settle(seq, rc) }
    }
    pub fn lane(&self, lane: u16, up: bool) {
        unsafe { ffi::ava1_send_test_lane(lane, up as c_int) }
    }
    pub fn received(&self, seq: u32) {
        unsafe { ffi::ava1_send_test_received(seq) }
    }
    pub fn credit(&self, n: u64) {
        unsafe { ffi::ava1_send_test_credit(n) }
    }
    /// The job is ending (j->stopping).
    pub fn stopping(&self) {
        unsafe { ffi::ava1_send_test_stopping() }
    }
    pub fn state(&self) -> CSendState {
        let mut o = [0u64; 4];
        unsafe { ffi::ava1_send_test_state(o.as_mut_ptr()) };
        CSendState {
            credit: o[0],
            ready: o[1],
            queued: o[2],
            inflight: o[3],
        }
    }
}

impl Drop for CSendWindow {
    fn drop(&mut self) {
        unsafe { ffi::ava1_send_test_end() };
    }
}

/// Counters the C test hooks keep while an apply job runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    /// Part files preallocated.
    pub prealloc_calls: u64,
    /// ... of which with the job mutex held (the slow part under the lock this guards against).
    pub prealloc_with_job_mutex_held: u64,
    /// Large-file commits begun.
    pub commits: u64,
    /// ... of which on the job thread (a batch's commits must run on the workers).
    pub commits_on_job_thread: u64,
    /// Directory syncs of sync batches (hook 7), how many ran on a worker, and by how many threads.
    pub batch_dir_syncs: u64,
    pub batch_dir_syncs_on_workers: u64,
    pub batch_dir_sync_threads: u64,
    /// The job switched to an fsync after every chunk (a drive too slow for batch fsyncs).
    pub per_chunk_fsync: bool,
}

/// Durable-by-log options for the next job begun or opened (zero = the engine's defaults; the
/// test default is off unless AVA1_TEST_LOG_SMALL is set in the environment).
#[derive(Debug, Clone, Copy, Default)]
pub struct LogOpts {
    /// 0 default, 1 on, 2 off.
    pub mode: u32,
    pub pack_segment: u32,
    pub unswept_max: u64,
    pub sweep_age_ms: u32,
    /// The cap across all jobs (0 = the default).
    pub unswept_total: u64,
    /// How often housekeeping recovers job directories nobody holds (0 = the default, 10 s).
    pub recover_every_ms: u32,
    /// Job directories one recovery pass takes (0 = the default).
    pub recover_max: u32,
    /// The first byte of the test job's id repeated (0 = 7).
    pub job_byte: u8,
}

impl LogOpts {
    pub const ON: LogOpts = LogOpts {
        mode: 1,
        pack_segment: 0,
        unswept_max: 0,
        sweep_age_ms: 0,
        unswept_total: 0,
        recover_every_ms: 0,
        recover_max: 0,
        job_byte: 0,
    };
    pub const OFF: LogOpts = LogOpts {
        mode: 2,
        ..LogOpts::ON
    };

    /// Hands the options to the C shim (the next job begun or opened takes them). The console's
    /// durable-by-log off-switch file is this thread's `c_set_log_small_flag` (default: the real path).
    pub fn apply(&self) {
        LOG_FLAG.with(|f| {
            let f = f.borrow();
            unsafe {
                ffi::ava1_test_set_log_small_flag(
                    f.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                )
            }
        });
        let v = [
            self.mode as u64,
            self.pack_segment as u64,
            self.unswept_max,
            self.sweep_age_ms as u64,
            self.unswept_total,
            self.recover_every_ms as u64,
            self.recover_max as u64,
            self.job_byte as u64,
        ];
        unsafe { ffi::ava1_test_apply_opts2(v.as_ptr()) }
    }
}

/// The payload's apply engine on a hand-built job (one at a time: it shares the C
/// server lock, since both use the data layer's globals).
pub struct CApplyJob {
    _lock: MutexGuard<'static, ()>,
}

impl CApplyJob {
    pub fn begin(
        jobs: &Path,
        root: &Path,
        flags: u32,
        m: &ava1::manifest::Manifest,
        fsync_delay_us: u32,
    ) -> Self {
        Self::begin_crash(jobs, root, flags, m, fsync_delay_us, 0)
    }

    pub fn begin_crash(
        jobs: &Path,
        root: &Path,
        flags: u32,
        m: &ava1::manifest::Manifest,
        fsync_delay_us: u32,
        crash_at: i32,
    ) -> Self {
        Self::begin_opts(
            jobs,
            root,
            flags,
            m,
            fsync_delay_us,
            crash_at,
            LogOpts::default(),
        )
    }

    pub fn begin_opts(
        jobs: &Path,
        root: &Path,
        flags: u32,
        m: &ava1::manifest::Manifest,
        fsync_delay_us: u32,
        crash_at: i32,
        opts: LogOpts,
    ) -> Self {
        let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        opts.apply();
        let dir = std::env::temp_dir().join(format!("ava1-blob-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ava1::journal::write_manifest(&dir, m).unwrap();
        let blob = std::fs::read(dir.join("manifest")).unwrap();
        let (j, r) = (
            CString::new(jobs.to_str().unwrap()).unwrap(),
            CString::new(root.to_str().unwrap()).unwrap(),
        );
        let rc = unsafe {
            ffi::ava1_test_apply_begin(
                j.as_ptr(),
                r.as_ptr(),
                flags,
                blob.as_ptr(),
                blob.len(),
                fsync_delay_us,
                crash_at,
            )
        };
        assert_eq!(rc, 0, "apply_begin");
        // begin reset the hook to "same"; install this thread's answer before any data.
        let v = SAME_DEVICE.with(|c| c.replace(1));
        unsafe { ffi::ava1_test_set_same_device(v) };
        CApplyJob { _lock: lock }
    }

    /// Adopts a held C server lock (the receiver opens its own job).
    pub fn from_lock(lock: MutexGuard<'static, ()>) -> Self {
        CApplyJob { _lock: lock }
    }

    pub fn chunk(&self, id: u32, off: u64, d: &[u8]) {
        assert_eq!(self.try_chunk(id, off, d), 0);
    }

    /// While on, the job thread starts no sync batch (so a test decides what one batch holds).
    pub fn hold_batches(&self, on: bool) {
        unsafe { ffi::ava1_test_apply_hold(on as c_int) }
    }

    /// Fails the next `n` fsync tries with `err` (an errno value, or Sony's 0x80020002):
    /// at once when `point` is None, else when the apply engine next reaches that hook.
    pub fn fault_fsync(&self, point: Option<i32>, n: i32, err: i32) {
        unsafe { ffi::ava1_test_fsync_fault(point.unwrap_or(-1), n, err) }
    }

    /// fsync tries (every file, directory and journal sync) made by the C engine since the process started.
    pub fn fsync_calls(&self) -> u32 {
        unsafe { ffi::ava1_test_fsync_calls() }
    }

    /// fsync retries made by the C engine since the process started.
    pub fn fsync_retries(&self) -> u32 {
        unsafe { ffi::ava1_test_fsync_retries() }
    }

    /// Faults armed and not yet used.
    pub fn fsync_faults_left(&self) -> i32 {
        unsafe { ffi::ava1_test_fsync_pending_faults() }
    }

    /// Waits until `n` small files are written and waiting for their batch.
    pub fn wait_pending(&self, n: u32, ms: u64) {
        let t = std::time::Instant::now();
        while unsafe { ffi::ava1_test_apply_pending() } < n {
            assert!(
                t.elapsed().as_millis() < ms as u128,
                "only {} pending of {n}",
                unsafe { ffi::ava1_test_apply_pending() }
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The directory sync after the rename of file `id` (u32::MAX: the staged tree's) fails
    /// with EIO.
    pub fn fail_dir_sync(&self, id: u32) {
        unsafe { ffi::ava1_test_apply_fail_dir_sync(id) }
    }

    /// ava1_apply_chunk's answer (0, or a negative AVA1_E_*).
    pub fn try_chunk(&self, id: u32, off: u64, d: &[u8]) -> i32 {
        unsafe { ffi::ava1_test_apply_chunk(id, off, d.as_ptr(), d.len()) }
    }

    pub fn record(&self, id: u32, d: &[u8], root: [u8; 32]) {
        assert_eq!(self.try_record(id, d, root), 0);
    }

    /// ava1_apply_bundle's answer for a one-record bundle.
    pub fn try_record(&self, id: u32, d: &[u8], root: [u8; 32]) -> i32 {
        unsafe { ffi::ava1_test_apply_record(id, d.as_ptr(), d.len(), root.as_ptr()) }
    }

    /// ava1_apply_bundle's answer for raw record bytes claiming `count` records.
    pub fn raw_bundle(&self, records: &[u8], count: u32) -> i32 {
        unsafe { ffi::ava1_test_apply_bundle_raw(records.as_ptr(), records.len(), count) }
    }

    /// What the hooks have seen since this job began (see `Probe`).
    pub fn probe(&self) -> Probe {
        let mut o = [0u64; 8];
        unsafe { ffi::ava1_test_apply_probe(o.as_mut_ptr()) };
        Probe {
            prealloc_calls: o[0],
            prealloc_with_job_mutex_held: o[1],
            commits: o[2],
            commits_on_job_thread: o[3],
            batch_dir_syncs: o[4],
            batch_dir_syncs_on_workers: o[5],
            batch_dir_sync_threads: o[6],
            per_chunk_fsync: o[7] != 0,
        }
    }

    /// Files done but not yet durable in place (the job's `unswept`).
    pub fn unswept(&self) -> u32 {
        unsafe { ffi::ava1_test_apply_unswept() }
    }

    /// Pack bytes counted against the cross-job cap, all jobs together (the data layer is one process-wide
    /// counter, so this is a method: only the test holding the C server lock may look at it).
    pub fn unswept_total(&self) -> u64 {
        unsafe { ffi::ava1_test_unswept_total() }
    }

    /// Pins (or releases, negative) bytes on the cross-job counter: other jobs' stuck log bytes.
    pub fn pin_unswept_total(&self, delta: i64) {
        unsafe { ffi::ava1_test_unswept_global_add(delta) }
    }

    /// Pack bytes of files not yet swept (pending ones included).
    pub fn unswept_bytes(&self) -> u64 {
        unsafe { ffi::ava1_test_apply_unswept_bytes() }
    }

    /// Pack segment files currently on disk in the job directory.
    pub fn segments(&self) -> u32 {
        unsafe { ffi::ava1_test_apply_segments() }
    }

    /// The next `n` file syncs of a sweep fail with EIO (-1: until set to 0).
    pub fn fail_sweeps(&self, n: i32) {
        unsafe { ffi::ava1_test_sweep_fail(n) }
    }

    /// While on, every commit waits as it is verified (so commits stay in flight).
    pub fn hold_commits(&self, on: bool) {
        unsafe { ffi::ava1_test_apply_hold_commit(on as c_int) }
    }

    /// The preallocation of file `id` answers ENOSPC.
    pub fn fault_prealloc(&self, id: u32) {
        unsafe { ffi::ava1_test_apply_fault_prealloc(id) }
    }

    /// Runs a journal compaction now: 0 compacted, -1 skipped.
    pub fn compact_now(&self) -> i32 {
        unsafe { ffi::ava1_test_apply_compact() }
    }

    /// Commits queued or running.
    pub fn commits_inflight(&self) -> u32 {
        unsafe { ffi::ava1_test_apply_commits_inflight() }
    }

    /// The one-line summary the job prints when it ends (where its time went).
    pub fn summary(&self) -> String {
        let mut b = vec![0u8; 1024];
        let n = unsafe { ffi::ava1_test_apply_summary(b.as_mut_ptr(), b.len()) };
        String::from_utf8_lossy(&b[..n]).into_owned()
    }

    /// Whether this job's periodic stats line is on (the timing opt-in, read at its start).
    pub fn timing_on(&self) -> bool {
        unsafe { ffi::ava1_test_apply_timing() != 0 }
    }

    /// The same for prepare's directory syncs: (calls, on a worker, distinct threads).
    pub fn probe_prepare_dirs(&self) -> (u64, u64, u64) {
        let mut o = [0u64; 3];
        unsafe { ffi::ava1_test_apply_probe_prep(o.as_mut_ptr()) };
        (o[0], o[1], o[2])
    }

    /// Makes every directory sync the engine reports (hooks 7 and 10) take `ms` more, so
    /// whether they run concurrently shows. Reset by the next open and by the end.
    pub fn hook_sleep(&self, ms: u32) {
        unsafe { ffi::ava1_test_apply_hook_sleep(ms) }
    }

    /// Record the apply engine's test hooks as "hook <point> <file id>" event lines.
    pub fn trace(&self, on: bool) {
        unsafe { ffi::ava1_test_apply_trace(on as c_int) }
    }

    /// Once file `id`'s root is verified at commit, and before the commit goes on, apply
    /// this chunk again (a late duplicate racing the commit).
    pub fn dup_on_commit(&self, id: u32, off: u64, d: &[u8]) {
        assert_eq!(
            unsafe { ffi::ava1_test_apply_dup_on_commit(id, off, d.as_ptr(), d.len()) },
            0
        );
    }

    /// Stops and frees the job now (still under the lock) and returns every event it
    /// emitted.
    pub fn end(self) -> String {
        unsafe { ffi::ava1_test_apply_end() };
        self.events()
    }

    pub fn root(&self, id: u32, root: [u8; 32]) {
        assert_eq!(unsafe { ffi::ava1_test_apply_root(id, root.as_ptr()) }, 0);
    }

    pub fn wait(&self, ms: u32) -> i32 {
        unsafe { ffi::ava1_test_apply_wait(ms) }
    }

    pub fn events(&self) -> String {
        let mut b = vec![0u8; 1 << 16];
        let n = unsafe { ffi::ava1_test_apply_events(b.as_mut_ptr(), b.len()) };
        String::from_utf8_lossy(&b[..n]).into_owned()
    }

    pub fn wait_event(&self, needle: &str, ms: u64) {
        let t = std::time::Instant::now();
        while !self.events().contains(needle) {
            assert!(
                t.elapsed().as_millis() < ms as u128,
                "no {needle:?} in {}",
                self.events()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

impl Drop for CApplyJob {
    fn drop(&mut self) {
        unsafe { ffi::ava1_test_apply_end() }
    }
}

fn recv_open_raw(
    jobs: &Path,
    root: &Path,
    flags: u32,
    policy: u8,
    entries: u32,
    crash_at: i32,
) -> i32 {
    let (j, r) = (
        CString::new(jobs.to_str().unwrap()).unwrap(),
        CString::new(root.to_str().unwrap()).unwrap(),
    );
    unsafe { ffi::ava1_test_recv_open(j.as_ptr(), r.as_ptr(), flags, policy, entries, crash_at) }
}

/// What a JobOpen carries besides the root.
#[derive(Clone, Copy, Debug)]
pub struct OpenArgs {
    pub kind: u8,
    pub flags: u32,
    pub policy: u8,
    pub entries: u32,
}

impl OpenArgs {
    pub const fn entries(self, n: u32) -> Self {
        OpenArgs { entries: n, ..self }
    }
}

/// The receiver driven directly (no network). It shares the data layer's lock and the apply
/// calls of `CApplyJob` (chunk, record, root, wait, events), which act on its job.
pub struct CRecv {
    inner: CApplyJob,
}

impl CRecv {
    pub fn open(jobs: &Path, root: &Path, flags: u32, policy: u8, crash_at: i32) -> Self {
        Self::open_opts(jobs, root, flags, policy, crash_at, LogOpts::default())
    }

    pub fn open_opts(
        jobs: &Path,
        root: &Path,
        flags: u32,
        policy: u8,
        crash_at: i32,
        opts: LogOpts,
    ) -> Self {
        let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        opts.apply();
        let inner = CApplyJob::from_lock(lock); // from here on, Drop stops the data layer
        assert_eq!(
            recv_open_raw(jobs, root, flags, policy, 0, crash_at),
            0,
            "recv_open"
        );
        CRecv { inner }
    }

    pub fn ack_staged(&self) -> u8 {
        unsafe { ffi::ava1_test_recv_staged() }
    }

    pub fn manifest(&self, m: &ava1::manifest::Manifest) {
        self.manifest_with_hash(m, m.hash())
    }

    pub fn manifest_with_hash(&self, m: &ava1::manifest::Manifest, hash: [u8; 32]) {
        use ava1::wire::Message;
        for p in m.pages([7; 16]) {
            let b = p.to_bytes().unwrap();
            assert_eq!(unsafe { ffi::ava1_test_recv_page(b.as_ptr(), b.len()) }, 0);
        }
        assert_eq!(
            unsafe { ffi::ava1_test_recv_end(m.files(), m.bytes(), hash.as_ptr()) },
            0
        );
    }

    /// The fast path: `Resume{manifest_hash}`.
    pub fn resume(&self, hash: [u8; 32]) {
        assert_eq!(unsafe { ffi::ava1_test_recv_resume(hash.as_ptr()) }, 0);
    }

    /// Stops the data layer and nothing else; how long that took.
    pub fn stop_data_timed(&self) -> std::time::Duration {
        let t = std::time::Instant::now();
        unsafe { ffi::ava1_test_data_stop_only() };
        t.elapsed()
    }

    /// A helper restart with no JobOpen after it: only the start-time recovery runs.
    pub fn restart_without_open(self) -> Self {
        assert_eq!(unsafe { ffi::ava1_test_recv_restart_noopen() }, 0);
        self
    }

    /// Housekeeping's reap as if an hour had passed, then the job is dropped: a settling job is
    /// destroyed with its files unswept (its directory stays).
    pub fn reap_and_drop(&self) {
        unsafe { ffi::ava1_test_reap_and_drop() }
    }

    /// Simulates a payload restart; the returned value replaces `self`.
    pub fn restart(self, crash_at: i32) -> Self {
        assert_eq!(
            unsafe { ffi::ava1_test_recv_restart(crash_at) },
            0,
            "reopen"
        );
        self
    }

    /// A restart whose JobOpen may be refused (see `last_open`).
    pub fn restart_any(self, crash_at: i32) -> Self {
        unsafe { ffi::ava1_test_recv_restart(crash_at) };
        self
    }

    /// The last JobOpen's status (0 = open).
    pub fn last_open(&self) -> i32 {
        unsafe { ffi::ava1_test_recv_last_open() }
    }

    /// Another JobOpen for the job, without a restart. With `drop_old` the previous
    /// session's reference is released first; refused, the previous one stays current.
    pub fn reopen(&self, drop_old: bool) -> i32 {
        unsafe { ffi::ava1_test_recv_reopen(drop_old as c_int) }
    }

    pub fn ack_credit(&self) -> u64 {
        unsafe { ffi::ava1_test_recv_ack_credit() }
    }

    fn set(&self, root: Option<&Path>, owner: u8, deny: i32) {
        let r = root.map(|p| CString::new(p.to_str().unwrap()).unwrap());
        unsafe {
            ffi::ava1_test_recv_set(
                r.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                owner,
                deny,
            )
        }
    }

    /// The root the next JobOpen names.
    pub fn set_root(&self, root: &Path) {
        self.set(Some(root), 0, -1)
    }

    /// The owner key (its first byte; 1 is the job's) the next JobOpen comes from.
    pub fn set_owner(&self, b: u8) {
        self.set(None, b, -1)
    }

    /// The data layer's may_write hook refuses every path.
    pub fn deny_write(&self, on: bool) {
        self.set(None, 0, on as i32)
    }

    /// Credit held as if a frame were in memory.
    pub fn reserve(&self, n: usize) {
        assert_eq!(unsafe { ffi::ava1_test_apply_reserve(n, 1) }, 0);
    }

    pub fn unreserve(&self, n: usize) {
        unsafe { ffi::ava1_test_apply_reserve(n, 0) };
    }

    pub fn wait_stopped(&self, ms: u64) {
        let t = std::time::Instant::now();
        while unsafe { ffi::ava1_test_job_stopped() } == 0 {
            assert!(
                t.elapsed().as_millis() < ms as u128,
                "the injected crash never fired"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Waits for `needle` and returns the event log.
    pub fn wait_event(&self, needle: &str, ms: u64) -> String {
        self.inner.wait_event(needle, ms);
        self.inner.events()
    }
}

impl std::ops::Deref for CRecv {
    type Target = CApplyJob;
    fn deref(&self) -> &CApplyJob {
        &self.inner
    }
}

/// JobOpen's status (0 when accepted); the job, if opened, is stopped again.
pub fn c_recv_open_status(jobs: &Path, root: &Path, a: OpenArgs) -> i32 {
    let lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let _job = CApplyJob::from_lock(lock);
    unsafe { ffi::ava1_test_recv_args(a.kind) };
    let rc = recv_open_raw(jobs, root, a.flags, a.policy, a.entries, 0);
    unsafe { ffi::ava1_test_recv_args(ava1::gen::JOB_UPLOAD) };
    rc
}

/// The management dispatcher's stub table (P3 Task 2): installs it for the C server's RPC.
pub mod mgmt {
    use super::ffi;

    /// Installs the stub table. 0 on success.
    pub fn install() -> i32 {
        unsafe { ffi::ava1_test_mgmt_install() }
    }
    /// What a table with a repeated method does at install (-1 expected).
    pub fn install_duplicate() -> i32 {
        unsafe { ffi::ava1_test_mgmt_install_duplicate() }
    }
    /// Removes the table (the server then advertises no CAP_MGMT when it starts).
    pub fn uninstall() {
        unsafe { ffi::ava1_test_mgmt_uninstall() }
    }
    /// `mgmt_status_for_token`.
    pub fn status_for_token(t: &str) -> i32 {
        let c = std::ffi::CString::new(t).unwrap();
        unsafe { ffi::ava1_test_mgmt_status_for_token(c.as_ptr()) }
    }
    /// How many entries the stub `app.list` handler produces.
    pub fn set_apps(n: u32) {
        unsafe { ffi::ava1_test_mgmt_set_apps(n) }
    }
    /// (enter calls, leave calls, last legacy frame, peak concurrent Sony-lock holders).
    pub fn stats() -> (u32, u32, u32, i32) {
        let (mut e, mut l, mut f, mut p) = (0, 0, 0, 0);
        unsafe { ffi::ava1_test_mgmt_stats(&mut e, &mut l, &mut f, &mut p) };
        (e, l, f, p)
    }
    /// Installs the Task 9 diagnostics table (klog, syslog, net.*, proc.modules) instead of the
    /// general stub table. 0 on success.
    pub fn install_diag() -> i32 {
        unsafe { ffi::ava1_test_mgmt_install_diag() }
    }
    /// The most diagnostics calls the console ran at once since `install_diag`.
    pub fn diag_peak() -> i32 {
        unsafe { ffi::ava1_test_mgmt_diag_peak() }
    }
    /// The stub `log.syslog` handler: `len` bytes of numbered lines (mode 0), the sysctl error
    /// frame (1), an empty buffer (2) or text after a 60 ms hold (3).
    pub fn set_syslog(len: u32, mode: i32) {
        unsafe { ffi::ava1_test_mgmt_set_syslog(len, mode) }
    }
    /// `mgmt_tail_window`: (clipped, start of the window).
    pub fn tail_window(text: &[u8], cap: usize) -> (bool, usize) {
        let mut start = 0usize;
        let c = unsafe { ffi::ava1_test_tail_window(text.as_ptr(), text.len(), cap, &mut start) };
        (c != 0, start)
    }
    /// The path the stub `fs.mkdir` handler last received.
    pub fn last_path() -> String {
        let mut b = [0u8; 256];
        let n = unsafe { ffi::ava1_test_mgmt_last_path(b.as_mut_ptr(), b.len()) };
        String::from_utf8_lossy(&b[..n]).into_owned()
    }
}

/// The AVA1 event log (payload/ava1/ava1_events.c, P3 Task 9).
pub mod events {
    use super::ffi;
    use std::ffi::CString;
    use std::path::Path;

    /// Points the log at `path` (None = off) with a roll size of `limit` bytes (0 = 1 MiB).
    pub fn set(path: Option<&Path>, limit: u32) {
        let c = path.map(|p| CString::new(p.to_str().unwrap()).unwrap());
        unsafe {
            ffi::ava1_test_events_set(c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), limit)
        }
    }
    /// `ava1_log_event`.
    pub fn log(line: &str) {
        let c = CString::new(line).unwrap();
        unsafe { ffi::ava1_test_events_log(c.as_ptr()) }
    }
}

/// P3 Task 7: the management table rows of the hardware/system/accounts/cheats/mods/notices/Remote
/// Play group, expanded from the real `mgmt_table.def` over stub handlers (`csrc/test_shim_t7.c`).
pub mod t7 {
    use super::ffi;

    /// One row of the real table: AVA1 method, `MGMT_*` flags, legacy request and ack frame numbers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Row {
        pub method: u32,
        pub flags: u32,
        pub frame: u32,
        pub ack: u32,
    }

    /// `MGMT_SONY`.
    pub const SONY: u32 = 1;

    /// Installs the Task 7 table (replacing any other). 0 on success.
    pub fn install() -> i32 {
        unsafe { ffi::ava1_test_t7_install() }
    }
    pub fn rows() -> Vec<Row> {
        let n = unsafe { ffi::ava1_test_t7_count() };
        (0..n)
            .map(|i| {
                let (mut m, mut f, mut fr, mut a) = (0, 0, 0, 0);
                assert_eq!(
                    unsafe { ffi::ava1_test_t7_entry(i, &mut m, &mut f, &mut fr, &mut a) },
                    0
                );
                Row {
                    method: m,
                    flags: f,
                    frame: fr,
                    ack: a,
                }
            })
            .collect()
    }
    /// The most threads that were inside a Sony-flagged stub (holding the real `sony_api_lock`) at once.
    pub fn sony_peak() -> i32 {
        unsafe { ffi::ava1_test_t7_sony_peak() }
    }
    pub fn reset_peak() {
        unsafe { ffi::ava1_test_t7_reset_peak() }
    }
}

/// Holds the Sony API lock on another thread for `ms`, as a worker inside a Sony call does. Returns once
/// the lock is held; join the handle to know it was released.
pub fn sony_hold(ms: u64) -> std::thread::JoinHandle<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        unsafe { ffi::ava1_test_sony_lock() };
        tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(ms));
        unsafe { ffi::ava1_test_sony_unlock() };
    });
    rx.recv().unwrap();
    h
}

/// A JobOpen for the job whose id is `byte` repeated, root `root`, as a session would send it: 0 when it
/// opened (the job is freed again), else the refusal's status.
pub fn probe_open(byte: u8, root: &Path) -> i32 {
    let r = CString::new(root.to_str().unwrap()).unwrap();
    unsafe { ffi::ava1_test_probe_open(byte, r.as_ptr()) }
}

/// Sweep failures still armed (see `sweep_failures`).
pub fn sweep_failures_left() -> i32 {
    unsafe { ffi::ava1_test_sweep_fail_left() }
}

/// Housekeeping loop iterations since the data layer started.
pub fn house_ticks() -> u32 {
    unsafe { ffi::ava1_test_house_ticks() }
}

/// The next `n` file syncs of any console sweep fail with EIO (-1: until set to 0): the wire tests' lever for a
/// receiver that cannot make its files durable.
pub fn sweep_failures(n: i32) {
    unsafe { ffi::ava1_test_sweep_fail(n) }
}

/// Sets the boot identity the GC strikes use (0 = the real one).
pub fn gc_boot(id: u64) {
    unsafe { ffi::ava1_test_gc_boot(id) }
}

/// ava1_exit_flush with its thread creation failing (or not): its result.
pub fn exit_flush_create_fails(fail: bool) -> i32 {
    unsafe { ffi::ava1_test_exit_flush_create_fails(fail as i32) }
}

/// `ava1_jobs_gc` over `jobs` as if `age_s` seconds had passed and the limit were `max_age_s`: how
/// many job directories it removed.
pub fn jobs_gc(jobs: &Path, age_s: i64, max_age_s: i64) -> i32 {
    let _lock = C_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let j = CString::new(jobs.to_str().unwrap()).unwrap();
    unsafe { ffi::ava1_test_jobs_gc(j.as_ptr(), age_s, max_age_s) }
}

/// Issue #354: payload/src/fan_map.c, the fan curve -> ICC threshold mapping (-1: no readable point).
pub fn fan_map_threshold(points_json: &str) -> i32 {
    extern "C" {
        fn fan_map_threshold(json: *const std::os::raw::c_char) -> std::os::raw::c_int;
    }
    let c = CString::new(points_json).expect("no NUL in a curve body");
    unsafe { fan_map_threshold(c.as_ptr()) }
}

/// payload/src/rp_pair.c, the Remote Play pairing state, over the fake Sony functions in
/// csrc/rp_pair_shim.c. One shared instance: callers serialise (tests/rp_pair.rs holds a lock).
pub mod rp_pair {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int};

    pub const IDLE: i32 = 0;
    pub const WAITING: i32 = 1;
    pub const PAIRED: i32 = 2;
    pub const FAILED: i32 = 3;
    pub const TIMEOUT: i32 = 4;
    /// RP_PAIR_WAIT_MS.
    pub const WAIT_MS: i64 = 300 * 1000;

    extern "C" {
        fn rpt_reset();
        fn rpt_set_prepare(rc: c_int);
        fn rpt_set_gen(rc: c_int, pin: u32);
        fn rpt_set_devices(n: c_int);
        fn rpt_set_confirm(rc: c_int, status: u32, err: u32);
        fn rpt_block_confirm(on: c_int);
        fn rpt_wait_in_confirm(ms: c_int) -> c_int;
        fn rpt_request(now_ms: i64) -> c_int;
        fn rpt_poll(now_ms: i64) -> c_int;
        fn rpt_cancel() -> c_int;
        fn rpt_settle();
        #[allow(clippy::too_many_arguments)]
        fn rpt_view(
            now_ms: i64,
            state: *mut c_int,
            seconds_left: *mut c_int,
            pin: *mut c_char,
            pin_cap: usize,
            err: *mut c_char,
            err_cap: usize,
            probes: *mut u32,
            last_status: *mut u32,
        );
        fn rpt_counts(out: *mut c_int);
        fn rpt_last_notify() -> *const c_char;
        fn rpt_state_name(s: c_int) -> *const c_char;
    }

    /// What the payload would report.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct View {
        pub state: i32,
        pub seconds_left: i32,
        pub pin: String,
        pub err: String,
        pub probes: u32,
        pub last_status: u32,
    }

    /// How often each fake Sony call ran.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Counts {
        pub prepare: i32,
        pub gen_pin: i32,
        pub confirm: i32,
        pub invalidate: i32,
        pub device_count: i32,
        pub notify: i32,
    }

    pub fn reset() {
        unsafe { rpt_reset() }
    }
    pub fn set_prepare(rc: i32) {
        unsafe { rpt_set_prepare(rc) }
    }
    pub fn set_gen(rc: i32, pin: u32) {
        unsafe { rpt_set_gen(rc, pin) }
    }
    pub fn set_devices(n: i32) {
        unsafe { rpt_set_devices(n) }
    }
    pub fn set_confirm(rc: i32, status: u32, err: u32) {
        unsafe { rpt_set_confirm(rc, status, err) }
    }
    pub fn block_confirm(on: bool) {
        unsafe { rpt_block_confirm(on as c_int) }
    }
    pub fn wait_in_confirm(ms: i32) -> bool {
        unsafe { rpt_wait_in_confirm(ms) != 0 }
    }
    pub fn request(now_ms: i64) -> i32 {
        unsafe { rpt_request(now_ms) }
    }
    pub fn poll(now_ms: i64) -> i32 {
        unsafe { rpt_poll(now_ms) }
    }
    /// True when the live PIN still has to be invalidated on the console.
    pub fn cancel() -> bool {
        unsafe { rpt_cancel() != 0 }
    }
    pub fn settle() {
        unsafe { rpt_settle() }
    }
    pub fn view(now_ms: i64) -> View {
        let (mut state, mut secs, mut probes, mut last) = (0, 0, 0u32, 0u32);
        let mut pin = [0 as c_char; 16];
        let mut err = [0 as c_char; 160];
        unsafe {
            rpt_view(
                now_ms,
                &mut state,
                &mut secs,
                pin.as_mut_ptr(),
                pin.len(),
                err.as_mut_ptr(),
                err.len(),
                &mut probes,
                &mut last,
            );
            View {
                state,
                seconds_left: secs,
                pin: CStr::from_ptr(pin.as_ptr()).to_string_lossy().into_owned(),
                err: CStr::from_ptr(err.as_ptr()).to_string_lossy().into_owned(),
                probes,
                last_status: last,
            }
        }
    }
    pub fn counts() -> Counts {
        let mut o = [0 as c_int; 6];
        unsafe { rpt_counts(o.as_mut_ptr()) };
        Counts {
            prepare: o[0],
            gen_pin: o[1],
            confirm: o[2],
            invalidate: o[3],
            device_count: o[4],
            notify: o[5],
        }
    }
    pub fn last_notify() -> String {
        unsafe {
            CStr::from_ptr(rpt_last_notify())
                .to_string_lossy()
                .into_owned()
        }
    }
    pub fn state_name(s: i32) -> String {
        unsafe {
            CStr::from_ptr(rpt_state_name(s))
                .to_string_lossy()
                .into_owned()
        }
    }
}
