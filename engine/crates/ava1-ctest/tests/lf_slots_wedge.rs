//! Many multi-chunk files under the console's small descriptor budget. A file's root comes on
//! the control connection and can arrive before its last chunks are applied; such a file could
//! neither commit nor be closed to free its descriptor slot, and once a job's whole share was
//! files like it every worker waited for a slot forever. A 35k-file game froze on two consoles.
mod common;

use std::time::{Duration, Instant};

use ava1_ctest::{CServer, LogOpts};
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn large_files_with_early_roots_never_wedge_the_descriptor_slots() {
    let d = dir("lf-slots-wedge");
    let src = d.join("src");
    // 300 files of 2-4 MiB (several chunks each) between small ones, like a game's assets.
    write_tree(&src, 900, |i| {
        if i % 3 == 0 {
            (2 << 20) + (i * 7919) % (2 << 20)
        } else {
            4096 + (i * 131) % (60 << 10)
        }
    });
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data_opts(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        3000, // a slow disk: files stay open long enough to pile up
        0,
        LogOpts::ON, // durable-by-log, as on the console
    );
    // Tighter than the console's ~490 so the job's share of slots fills at once.
    srv.knob("fd_budget", 120);
    // A wedge never finishes, so the bound only has to be well past a healthy run: about 65 s
    // here, several times that under ASan/UBSan (90 s timed out a healthy sanitizer run on CI).
    let bound = if cfg!(ava1_ctest_sanitize) { 900 } else { 300 };
    let t = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(bound),
        upload(
            &srv.addr(),
            me,
            mine,
            &src,
            d.join("dst").to_str().unwrap(),
            [0x31; 16],
            |_| {},
        ),
    )
    .await
    .expect("the upload froze with every worker waiting for a descriptor slot");
    eprintln!("900 files in {:.1} s", t.elapsed().as_secs_f64());
    assert_eq!(r.0.status, 0, "{:?}", r.0.message);
    assert!(same_tree(&src, &d.join("dst")));
}
