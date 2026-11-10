//! #783: the engine asks the console to read each large file back from its drive (AVA1
//! `JF_READBACK`) when the helper can and the setting is on. Loopback uploads to the C receiver:
//! a byte changed on the console's drive after it arrived is found, the file is sent again and
//! the job says so (loudly in the log, and in its commit ack); a file that keeps reading back
//! wrong fails the job with its own reason; an older helper (no `CAP_READBACK`) is never sent the
//! flag and the job says it was not checked; the setting off sends no flag either.
#![cfg(unix)]
mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use common::{dir, SECRET};
use ps5upload_ava1::upload::upload_dir_in;
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;
use ps5upload_ctest::CServer;

extern "C" {
    fn ava1_test_readback_cap(on: i32);
    fn ava1_test_readback_corrupt(n: i32, at: u64);
    fn ava1_test_readback_corrupt_left() -> i32;
    fn ava1_test_readback_last() -> i32;
}

const MIB: usize = 1 << 20;

/// The knobs and the log sink are process-wide: one test at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// The engine's log lines while a test runs.
fn log_lines() -> Arc<Mutex<Vec<String>>> {
    static LINES: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> = std::sync::OnceLock::new();
    let lines = LINES
        .get_or_init(|| {
            let l: Arc<Mutex<Vec<String>>> = Arc::default();
            let sink = l.clone();
            ps5upload_core::log::set_leveled_sink(move |_, _, msg| {
                sink.lock().unwrap().push(msg.to_string());
            });
            l
        })
        .clone();
    lines.lock().unwrap().clear();
    lines
}

struct Rig {
    _serial: MutexGuard<'static, ()>,
    t: ps5upload_ctest::TempDir,
    pool: Pool,
    srv: Option<CServer>,
}

impl Rig {
    /// A data server that advertises the read-back (`cap`) or not, and an engine pool paired with it.
    fn new(tag: &str, cap: bool) -> Rig {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            ava1_test_readback_cap(cap as i32);
            ava1_test_readback_corrupt(0, 0);
        }
        let t = dir(tag);
        let ava = t.join("ava");
        std::fs::create_dir_all(&ava).unwrap();
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        PeerStore::load(&t.join("srv-peers"))
            .unwrap()
            .add(me.public(), "engine")
            .unwrap();
        PeerStore::load(&ava.join("peers"))
            .unwrap()
            .add(Identity::from_secret(SECRET).public(), "C receiver")
            .unwrap();
        let srv = CServer::start_data(
            SECRET,
            &t.join("srv-peers"),
            &t.join("jobs"),
            200,
            5000,
            5000,
            0,
        );
        let pool = Pool::new(ava).with_addr(srv.addr());
        Rig {
            _serial: serial,
            t,
            pool,
            srv: Some(srv),
        }
    }

    fn src(&self, files: &[(&str, usize)]) -> std::path::PathBuf {
        let src = self.t.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (k, (name, size)) in files.iter().enumerate() {
            let b: Vec<u8> = (0..*size)
                .map(|i| (i as u8).wrapping_mul(7).wrapping_add(k as u8) ^ (i >> 16) as u8)
                .collect();
            std::fs::write(src.join(name), b).unwrap();
        }
        src
    }

    fn upload(
        &self,
        src: &Path,
        verify: bool,
        job: u8,
    ) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
        let mut c = TransferConfig::new("127.0.0.1:9120");
        c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
        c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
        c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
        c.cancel = Some(Arc::new(AtomicBool::new(false)));
        c.verify_writes = verify;
        upload_dir_in(
            &self.pool,
            &c,
            [job; 16],
            self.t.join("dest").to_str().unwrap(),
            src,
        )
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.srv.take();
        unsafe {
            ava1_test_readback_cap(1);
            ava1_test_readback_corrupt(0, 0);
        }
    }
}

fn same_tree(a: &Path, b: &Path, names: &[&str]) {
    for n in names {
        assert!(
            std::fs::read(a.join(n)).unwrap() == std::fs::read(b.join(n)).unwrap(),
            "{n} differs"
        );
    }
}

#[test]
fn a_byte_changed_on_the_console_drive_is_sent_again_and_the_job_says_so() {
    let lines = log_lines();
    let rig = Rig::new("rb-once", true);
    let src = rig.src(&[("big.bin", 6 * MIB + 123), ("small.txt", 1000)]);
    unsafe { ava1_test_readback_corrupt(1, 5 * MIB as u64 + 9) };
    let r = rig.upload(&src, true, 0x83).expect("the upload ends OK");
    assert_eq!(
        unsafe { ava1_test_readback_corrupt_left() },
        0,
        "the change was made"
    );
    assert_eq!(
        unsafe { ava1_test_readback_last() },
        1,
        "the job asked for the read-back"
    );
    same_tree(&src, &rig.t.join("dest"), &["big.bin", "small.txt"]);
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    assert_eq!(ack["readback"], "on");
    assert!(
        ack["rechecked_bytes"].as_u64().unwrap() >= (6 * MIB) as u64,
        "the whole file went again: {ack}"
    );
    let log = lines.lock().unwrap().join("\n");
    assert!(
        log.contains("did not match what was sent") && log.contains("sent again"),
        "the resend is in the engine log: {log}"
    );
}

#[test]
fn a_file_that_keeps_reading_back_wrong_fails_the_job_with_its_own_reason() {
    let rig = Rig::new("rb-always", true);
    let src = rig.src(&[("big.bin", 3 * MIB)]);
    unsafe { ava1_test_readback_corrupt(1000, 10) };
    let e = rig.upload(&src, true, 0x84).expect_err("the job fails");
    let f = e
        .downcast_ref::<ps5upload_ava1::upload::UploadFailure>()
        .unwrap_or_else(|| panic!("a typed failure: {e:#}"));
    assert_eq!(f.reason, "ava1_verify_failed", "{}", f.detail);
    assert!(
        !rig.t.join("dest/big.bin").exists(),
        "nothing wrong was committed"
    );
}

#[test]
fn an_older_helper_is_never_sent_the_flag_and_the_job_says_it_was_not_checked() {
    let lines = log_lines();
    let rig = Rig::new("rb-old", false);
    let src = rig.src(&[("big.bin", 4 * MIB + 1)]);
    unsafe { ava1_test_readback_corrupt(0, 0) }; // installs the probe hook, changes nothing
    let r = rig.upload(&src, true, 0x85).expect("the upload ends OK");
    assert_eq!(
        unsafe { ava1_test_readback_last() },
        0,
        "no JF_READBACK reached the console"
    );
    same_tree(&src, &rig.t.join("dest"), &["big.bin"]);
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    assert_eq!(ack["readback"], "unsupported");
    let log = lines.lock().unwrap().join("\n");
    assert!(
        log.contains("cannot read files back"),
        "the engine says the upload was not checked: {log}"
    );
}

#[test]
fn the_setting_off_sends_no_flag() {
    let rig = Rig::new("rb-off", true);
    let src = rig.src(&[("big.bin", 4 * MIB + 1)]);
    unsafe { ava1_test_readback_corrupt(0, 0) };
    let r = rig.upload(&src, false, 0x86).expect("the upload ends OK");
    assert_eq!(unsafe { ava1_test_readback_last() }, 0);
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    assert_eq!(ack["readback"], "off");
    same_tree(&src, &rig.t.join("dest"), &["big.bin"]);
}
