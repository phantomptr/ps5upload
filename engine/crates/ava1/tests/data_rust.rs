//! The Rust receiver end to end: a Rust sender against a Rust folder host, ordered
//! downloads, credit, resume from the engine's journal, and the host's path guard.
//! `OrderCheckSink` lives here, not in tests/common (the controller's file split for
//! this round: Task 16 owns the ava1-ctest tests/common).
mod common;

use std::sync::Arc;
use std::time::Duration;

use ava1::conn::Frame;
use ava1::gen::{
    self, Bundle, BundleRecord, Chunk, Credit, Durable, JobDone, JobMap, JobOpen, JobOpenAck,
    ManifestEnd, Received,
};
use ava1::host::FolderHost;
use ava1::journal::{self, Record};
use ava1::manifest::{self, Entry, Manifest};
use ava1::recv::{download_job, receive_job, LocalSink, RecvOptions, Sink};
use ava1::router::{Inbound, JobHost, JobLink};
use ava1::send::{send_job, SendOptions};
use ava1::session::connect;
use ava1::source::LocalSource;
use ava1::wire::{FrameMessage, Message};

/// Records every (file, offset) a download writes, checks the sequence is sorted, and
/// stores the bytes so the receiver's commit-time verification (which reads the groups
/// back) sees what was written — `read_at` returning zeros would fail the root check
/// for every one-group file and storm FileRetry.
#[derive(Default)]
struct OrderCheckSink {
    seen: std::sync::Mutex<Vec<(u32, u64)>>,
    bytes: std::sync::Mutex<std::collections::HashMap<u32, Vec<u8>>>,
}

impl OrderCheckSink {
    fn in_order(&self) -> bool {
        self.seen.lock().unwrap().windows(2).all(|w| w[0] < w[1])
    }
}

impl Sink for OrderCheckSink {
    fn prepare(&self, _m: &Manifest) -> std::io::Result<()> {
        Ok(())
    }
    fn write_at(&self, id: u32, off: u64, d: &[u8]) -> std::io::Result<()> {
        self.seen.lock().unwrap().push((id, off));
        let mut m = self.bytes.lock().unwrap();
        let b = m.entry(id).or_default();
        let end = off as usize + d.len();
        if b.len() < end {
            b.resize(end, 0);
        }
        b[off as usize..end].copy_from_slice(d);
        Ok(())
    }
    fn write_whole(&self, id: u32, d: &[u8]) -> std::io::Result<()> {
        self.seen.lock().unwrap().push((id, 0));
        self.bytes.lock().unwrap().insert(id, d.to_vec());
        Ok(())
    }
    fn sync(&self, _ids: &[u32]) -> std::io::Result<()> {
        Ok(())
    }
    fn read_at(&self, id: u32, off: u64, b: &mut [u8]) -> std::io::Result<usize> {
        let m = self.bytes.lock().unwrap();
        let Some(f) = m.get(&id) else { return Ok(0) };
        let start = off as usize;
        if start >= f.len() {
            return Ok(0);
        }
        let n = b.len().min(f.len() - start);
        b[..n].copy_from_slice(&f[start..start + n]);
        Ok(n)
    }
    fn commit(&self, _id: u32) -> std::io::Result<()> {
        Ok(())
    }
    fn finish(&self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tree(d: &std::path::Path) {
    for i in 0..500 {
        let p = d.join(format!("a{}/f{i}", i % 9));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let n = if i % 100 == 0 { (3 << 20) + i } else { i * 7 };
        std::fs::write(p, (0..n).map(|k| (k + i) as u8).collect::<Vec<_>>()).unwrap();
    }
}

fn same(a: &std::path::Path, b: &std::path::Path) {
    let ma = manifest::walk(&LocalSource::new(a.into()), &|_: &str| false).unwrap();
    let mb = manifest::walk(&LocalSource::new(b.into()), &|_: &str| false).unwrap();
    assert_eq!(ma.entries.len(), mb.entries.len());
    for e in &ma.entries {
        if e.kind == gen::ENTRY_FILE {
            assert_eq!(
                std::fs::read(a.join(&e.path)).unwrap(),
                std::fs::read(b.join(&e.path)).unwrap(),
                "{}",
                e.path
            );
        }
    }
}

fn opts(jobs: &std::path::Path, ordered: bool) -> RecvOptions {
    RecvOptions {
        credit: 64 << 20,
        flags: if ordered { gen::JF_ORDERED } else { 0 },
        jobs_dir: jobs.into(),
        ordered,
        progress: Arc::default(),
        cancel: Arc::default(),
        progress_deadline: None,
    }
}

/// The next control frame on the job's inbox. Every async wait in this file is bounded
/// (a missing signal must fail the test, not hang the round).
async fn next_control(link: &mut JobLink) -> Frame {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), link.rx.recv())
            .await
            .expect("no frame within 20 s")
            .expect("the job channel closed");
        if let Inbound::Control(f) = ev {
            return f;
        }
    }
}

/// The opener's upload handshake, hand-driven: JobOpen → ack → manifest pages → end →
/// map pages. The map accumulates into the returned `Need`-shaped pages.
async fn open_and_map(
    link: &mut JobLink,
    job: [u8; 16],
    root: &str,
    m: &Manifest,
) -> (JobOpenAck, ava1::ranges::Need) {
    link.control
        .send(&JobOpen {
            job_id: job,
            kind: gen::JOB_UPLOAD,
            policy: 0,
            flags: 0,
            root: root.into(),
            src: None,
            credit: None,
        })
        .await
        .unwrap();
    let ack: JobOpenAck = next_control(link).await.decode().unwrap();
    assert_eq!(ack.status, 0);
    for p in m.pages(job) {
        link.control.send(&p).await.unwrap();
    }
    link.control
        .send(&ManifestEnd {
            job_id: job,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    let mut need = ava1::ranges::Need::default();
    loop {
        let f = next_control(link).await;
        if f.ty != JobMap::TYPE {
            continue;
        }
        let p: JobMap = f.decode().unwrap();
        assert_eq!(p.status, 0);
        need.add_page(&p);
        if p.last == 1 {
            return (ack, need);
        }
    }
}

/// A host with a chosen credit, for the hand-driven receiver tests (the real host grants
/// 64 MiB; the credit tests need a small grant).
struct CreditHost {
    root: std::path::PathBuf,
    jobs: std::path::PathBuf,
    credit: u64,
}

impl JobHost for CreditHost {
    fn accept(&self, mut link: JobLink, first: Frame, _peer: [u8; 32]) {
        let (root, jobs, credit) = (self.root.clone(), self.jobs.clone(), self.credit);
        tokio::spawn(async move {
            let Ok(open) = first.decode::<JobOpen>() else {
                return;
            };
            let sink = Arc::new(LocalSink::new(
                root.join(&open.root),
                open.flags & gen::JF_SINGLE_FILE != 0,
            ));
            let o = RecvOptions {
                credit,
                flags: open.flags,
                jobs_dir: jobs,
                ordered: open.flags & gen::JF_ORDERED != 0,
                progress: Arc::default(),
                cancel: Arc::default(),
                progress_deadline: None,
            };
            let _ = receive_job(&mut link, open, sink, o).await;
        });
    }
}

/// A sealed Bundle frame for one small file, with the root the receiver's check expects.
fn bundle_body(job: [u8; 16], file_id: u32, data: &[u8]) -> Vec<u8> {
    Bundle {
        job_id: job,
        records: vec![BundleRecord {
            file_id,
            root: *blake3::hash(data).as_bytes(),
            data: data.to_vec(),
        }],
    }
    .to_bytes()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_to_rust_upload_into_a_folder_host() {
    let d = common::temp_dir("rr-up");
    tree(&d.join("src"));
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let src = LocalSource::new(d.join("src"));
    let m = Arc::new(manifest::walk(&src, &|_: &str| false).unwrap());
    let mut link = s.job([1; 16]);
    let r = send_job(&mut link, m, Arc::new(src), SendOptions::upload("in"))
        .await
        .unwrap();
    assert_eq!(r.status, 0);
    same(&d.join("src"), &d.join("share/in"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_to_rust_download_from_a_folder_host() {
    let d = common::temp_dir("rr-down");
    tree(&d.join("share/out"));
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([2; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = download_job(&mut link, "out", 0, sink, opts(&d.join("jobs"), false))
        .await
        .unwrap();
    assert_eq!(r.files, 500);
    same(&d.join("share/out"), &d.join("got"));
}

async fn ordered_download(tag: &str, job: u8, timing: ava1::session::Timing) {
    let d = common::temp_dir(tag);
    tree(&d.join("share/out"));
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) =
        common::paired_ctx(|c| c.with_jobs(host).with_timing(timing)).await;
    let s = connect(&addr.to_string(), id, peers, "client", timing)
        .await
        .unwrap();
    let mut link = s.job([job; 16]);
    let sink = Arc::new(OrderCheckSink::default());
    download_job(
        &mut link,
        "out",
        gen::JF_ORDERED,
        sink.clone(),
        opts(&d.join("jobs"), true),
    )
    .await
    .unwrap();
    assert!(
        sink.in_order(),
        "writes arrived out of (file, offset) order"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordered_download_reaches_the_sink_in_file_order() {
    ordered_download("rr-ordered", 3, common::fast()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_refuses_paths_outside_its_share() {
    let d = common::temp_dir("rr-escape");
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let src = LocalSource::new(d.clone());
    let m = Arc::new(Manifest::default());
    let mut link = s.job([4; 16]);
    let e = send_job(&mut link, m, Arc::new(src), SendOptions::upload("../x"))
        .await
        .unwrap_err();
    assert!(matches!(e, ava1::send::SendError::Refused { status, .. } if status == gen::ERR_PATH));
}

/// The §11.2 guard on the download direction: a peer-chosen root is refused before it
/// can reach `LocalSource` (ruling 14).
#[tokio::test(flavor = "multi_thread")]
async fn a_download_of_a_path_outside_the_share_is_refused_too() {
    let d = common::temp_dir("rr-descape");
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let job = [0x33; 16];
    let mut link = s.job(job);
    link.control
        .send(&JobOpen {
            job_id: job,
            kind: gen::JOB_DOWNLOAD,
            policy: 0,
            flags: 0,
            root: "../x".into(),
            src: None,
            credit: Some(1 << 20),
        })
        .await
        .unwrap();
    let ack: JobOpenAck = next_control(&mut link).await.decode().unwrap();
    assert_eq!(
        ack.status,
        gen::ERR_PATH,
        "the host refused before building a source"
    );
}

/// SPEC.md §12.4: a frame larger than the credit still outstanding is refused with the
/// sealed Error(ERR_CREDIT) on the offending lane, and only that lane ends: the session and
/// the job stay, and a second lane still delivers (Received). Nothing of the refused frame
/// is buffered or acknowledged.
#[tokio::test(flavor = "multi_thread")]
async fn an_over_credit_chunk_closes_only_its_lane() {
    let d = common::temp_dir("rr-credit");
    let host = Arc::new(CreditHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        credit: 2 << 20,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let job = [0x22; 16];
    let m = Manifest {
        entries: vec![Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size: 8 << 20,
            mtime: 0,
            path: "big".into(),
            root: None,
        }],
    };
    let mut link = s.job(job);
    let (ack, _map) = open_and_map(&mut link, job, "in", &m).await;
    assert_eq!(ack.credit, 2 << 20, "the grant is the job's credit");
    let lane_conn = s.open_lane().await.unwrap();
    let lane = lane_conn.id;
    let good_conn = s.open_lane().await.unwrap();
    let good = good_conn.id;
    let body = Chunk {
        job_id: job,
        file_id: 0,
        offset: 0,
        data: vec![0x5a; 4 << 20],
    }
    .to_bytes()
    .unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Chunk::TYPE, 0, 1, body)
        .await
        .unwrap();
    // The offending lane dies with the decoded sealed Error: code 17 is ERR_CREDIT.
    let why = tokio::time::timeout(Duration::from_secs(20), lane_conn.closed())
        .await
        .expect("the lane closed within 20 s");
    assert!(
        why.contains("error 17"),
        "the sealed ERR_CREDIT crossed: {why}"
    );
    // The session and the job go on: a valid chunk on the other lane is Received, and
    // nothing was Received for the refused frame (seq 1).
    let ok = Chunk {
        job_id: job,
        file_id: 0,
        offset: 0,
        data: vec![0x5a; 1 << 20],
    }
    .to_bytes()
    .unwrap();
    link.lane(good)
        .unwrap()
        .tx
        .send_raw(Chunk::TYPE, 0, 2, ok)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match link.rx.recv().await.expect("the job channel closed") {
                Inbound::Closed(why) => panic!("the session ended: {why}"),
                Inbound::Control(f) if f.ty == Received::TYPE => {
                    let r: Received = f.decode().unwrap();
                    assert_eq!(r.seq, 2, "Received only for the admitted frame");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("Received on the healthy lane within 20 s");
}

/// SPEC.md §12.4, the grant direction (ledger row 19): the sender's window is the grant
/// plus every Credit frame sent back, so the receiver must admit frames up to that
/// running total — never only up to the grant minus the credit already returned. With
/// the wrong (subtractive) check the first frame after the receiver has returned credit
/// trips ERR_CREDIT: a 16 MiB upload against a 4 MiB grant dies on the second or third
/// piece, while the correct check mirrors the sender's window exactly and the whole
/// upload completes through several credit returns.
#[tokio::test(flavor = "multi_thread")]
async fn an_upload_larger_than_its_grant_completes_through_credit_returns() {
    let d = common::temp_dir("rr-grant");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/big.bin"), vec![0x3c; 16 << 20]).unwrap();
    let host = Arc::new(CreditHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        credit: 4 << 20,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let src = LocalSource::new(d.join("src"));
    let m = Arc::new(manifest::walk(&src, &|_: &str| false).unwrap());
    let mut link = s.job([0x5a; 16]);
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        send_job(&mut link, m, Arc::new(src), SendOptions::upload("in")),
    )
    .await
    .expect("the upload finished within 30 s")
    .unwrap();
    assert_eq!(r.status, 0);
    same(&d.join("src"), &d.join("share/in"));
}

/// An empty file is a real file: the download must complete with the empty file present.
/// The receiver completes it on its own — the sender may send an empty bundle record for
/// it, or nothing at all (and the sender's own bundle flush can drop the record). The
/// wait is bounded: a missing completion must fail the test, not hang it.
#[tokio::test(flavor = "multi_thread")]
async fn a_zero_byte_file_completes_in_an_ordinary_download() {
    let d = common::temp_dir("rr-zero");
    std::fs::create_dir_all(d.join("share/out")).unwrap();
    std::fs::write(d.join("share/out/empty.bin"), b"").unwrap();
    std::fs::write(d.join("share/out/real.bin"), vec![0xa7; 1 << 20]).unwrap();
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([9; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        download_job(&mut link, "out", 0, sink, opts(&d.join("jobs"), false)),
    )
    .await
    .expect("the download completed within 30 s")
    .unwrap();
    assert_eq!(r.files, 2);
    same(&d.join("share/out"), &d.join("got"));
}

/// An empty file ahead of a real file in an ordered download: the ordered sender sends
/// every file as chunks and a zero range has none, so no frame can ever arrive for the
/// empty file — the receiver's cursor waited at it forever and the real file behind it
/// never applied. The wait is bounded: a stall must fail the test, not hang it.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_file_never_stalls_the_ordered_files_behind_it() {
    let d = common::temp_dir("rr-zero-ord");
    std::fs::create_dir_all(d.join("share/out")).unwrap();
    std::fs::write(d.join("share/out/empty.bin"), b"").unwrap();
    std::fs::write(d.join("share/out/real.bin"), vec![0xb7; 1 << 20]).unwrap();
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([10; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        download_job(
            &mut link,
            "out",
            gen::JF_ORDERED,
            sink,
            opts(&d.join("jobs"), true),
        ),
    )
    .await
    .expect("the ordered download completed within 30 s")
    .unwrap();
    assert_eq!(r.files, 2);
    same(&d.join("share/out"), &d.join("got"));
}

/// The zero-frame half of the empty-file fix, pinned without the sender: only the real
/// file's frames arrive — nothing is ever sent for the zero-byte file — and the job
/// still ends with both files done (the empty one on disk too).
#[tokio::test(flavor = "multi_thread")]
async fn a_file_with_no_frames_still_completes_the_job() {
    let d = common::temp_dir("rr-zero-frames");
    let host = Arc::new(CreditHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        credit: 1 << 20,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let job = [0x77; 16];
    let m = Manifest {
        entries: vec![
            Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 0,
                mtime: 0,
                path: "empty.bin".into(),
                root: None,
            },
            Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 0,
                path: "real.bin".into(),
                root: None,
            },
        ],
    };
    let mut link = s.job(job);
    let (ack, _map) = open_and_map(&mut link, job, "in", &m).await;
    assert_eq!(ack.credit, 1 << 20);
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle_body(job, 1, b"hey!"))
        .await
        .unwrap();
    let done: JobDone = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let f = next_control(&mut link).await;
            if f.ty == JobDone::TYPE {
                break f.decode().unwrap();
            }
        }
    })
    .await
    .expect("the job completed within 20 s");
    assert_eq!(done.status, 0);
    assert_eq!(done.files, 2);
    assert_eq!(std::fs::read(d.join("share/in/empty.bin")).unwrap(), b"");
    assert_eq!(std::fs::read(d.join("share/in/real.bin")).unwrap(), b"hey!");
}

/// The Windows half of the §11.2 guard: check_path splits on '/', so a backslash path
/// passes it on a Unix host — but Windows treats '\\' as a separator and
/// `PathBuf::join("a\\..\\..\\x")` escapes the share. The share-root check refuses
/// backslashes outright (a JobOpen.root never legitimately contains one).
#[tokio::test(flavor = "multi_thread")]
async fn a_host_refuses_backslash_paths() {
    let d = common::temp_dir("rr-bslash");
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let src = LocalSource::new(d.clone());
    let m = Arc::new(Manifest::default());
    let mut link = s.job([0x55; 16]);
    let e = send_job(
        &mut link,
        m,
        Arc::new(src),
        SendOptions::upload("a\\..\\..\\x"),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ava1::send::SendError::Refused { status, .. } if status == gen::ERR_PATH));
}

/// A job reopened with the same manifest resumes from the engine's journal: the map
/// carries the durable file, the journal's Open records exactly the sink's resume key,
/// the ack carries the job's ABSOLUTE grant again, and every Credit frame is just the
/// delta it freed — never a grant (the extra credit note: only the ack sets the window).
#[tokio::test(flavor = "multi_thread")]
async fn a_reopened_job_resumes_from_the_engine_journal_and_credits_are_deltas() {
    let d = common::temp_dir("rr-resume");
    let host = Arc::new(CreditHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        credit: 1 << 20,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let job = [0x11; 16];
    let m = Manifest {
        entries: vec![
            Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 0,
                path: "a".into(),
                root: None,
            },
            Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 0,
                path: "b".into(),
                root: None,
            },
        ],
    };
    let bundle0 = bundle_body(job, 0, b"one!");
    let bundle1 = bundle_body(job, 1, b"two!");

    // First session: the ack is the absolute grant, one file arrives, its batch is
    // journaled (Durable), and the Credit frame is the delta the frame freed.
    {
        let s = connect(
            &addr.to_string(),
            id.clone(),
            peers.clone(),
            "client",
            common::fast(),
        )
        .await
        .unwrap();
        let mut link = s.job(job);
        let (ack, map) = open_and_map(&mut link, job, "in", &m).await;
        assert_eq!(ack.credit, 1 << 20, "the ack is the absolute grant");
        assert!(map.done.is_empty());
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle0.clone())
            .await
            .unwrap();
        let recv: ava1::gen::Received = next_control(&mut link).await.decode().unwrap();
        assert_eq!(recv.lane, lane);
        let credit: Credit = next_control(&mut link).await.decode().unwrap();
        assert_eq!(
            credit.bytes,
            bundle0.len() as u64,
            "a Credit frame is the delta it freed, not the grant"
        );
        let durable: Durable = next_control(&mut link).await.decode().unwrap();
        assert_eq!(durable.files, vec![gen::FileRun { first: 0, count: 1 }]);
        drop(s); // the session ends mid-job: file 1 never arrived
    }

    // The journal on disk is the C receiver's layout: the Open records the sink's resume
    // key — the destination root and the construction-time staged decision (ruling Q1).
    let dir = journal::job_dir(&d.join("hjobs"), &job);
    let (_, recs) = journal::Journal::open(&dir).unwrap();
    let open_rec: gen::JnlOpen = match &recs[0] {
        Record::Open(o) => o.clone(),
        r => panic!("expected the Open record, got {r:?}"),
    };
    assert_eq!(
        open_rec.root,
        d.join("share/in").to_str().unwrap(),
        "the journal's root is the sink's resume-key root"
    );
    assert_eq!(
        open_rec.staged, 1,
        "staged = !single_file && !root.exists()"
    );
    assert_eq!(open_rec.flags, 0);
    assert_eq!(open_rec.manifest_hash, m.hash());

    // Second session (a resume via JobOpen, SPEC.md §11.5): the map carries the durable
    // file, the ack again carries the ABSOLUTE grant — nothing outstanding carries across
    // a reconnect — and the remaining file's Credit is again just its delta.
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job(job);
    let (ack, map) = open_and_map(&mut link, job, "in", &m).await;
    assert_eq!(
        ack.credit,
        1 << 20,
        "the resume ack is the absolute grant, not a remainder"
    );
    assert_eq!(
        map.done.iter().copied().collect::<Vec<_>>(),
        vec![0],
        "the journal's durable file is done on the wire"
    );
    assert!(map.partial.is_empty());
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle1.clone())
        .await
        .unwrap();
    let recv: ava1::gen::Received = next_control(&mut link).await.decode().unwrap();
    assert_eq!(recv.lane, lane);
    let credit: Credit = next_control(&mut link).await.decode().unwrap();
    assert_eq!(
        credit.bytes,
        bundle1.len() as u64,
        "the resumed job's Credit is again just the delta"
    );
    let done: JobDone = loop {
        let f = next_control(&mut link).await;
        if f.ty == JobDone::TYPE {
            break f.decode().unwrap();
        }
    };
    assert_eq!(done.status, 0);
    assert_eq!(std::fs::read(d.join("share/in/a")).unwrap(), b"one!");
    assert_eq!(std::fs::read(d.join("share/in/b")).unwrap(), b"two!");
}

/// Ruling Q3: the part file is staged next to the final path (same directory), so the
/// part→final rename can never cross a device — the placement is the guard.
#[test]
fn the_part_file_lives_in_the_final_files_parent_directory() {
    let d = common::temp_dir("rr-part");
    let root = d.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let sink = LocalSink::new(root.clone(), false);
    let m = Manifest {
        entries: vec![
            Entry {
                kind: gen::ENTRY_DIR,
                mode: 0o755,
                size: 0,
                mtime: 0,
                path: "a".into(),
                root: None,
            },
            Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 8,
                mtime: 0,
                path: "a/b".into(),
                root: None,
            },
        ],
    };
    sink.prepare(&m).unwrap();
    sink.write_at(1, 0, b"hello!!!").unwrap();
    assert!(
        root.join("a/b.ava-part").exists(),
        "the part file is in the final file's parent directory"
    );
    assert!(!root.join("a/b").exists());
    sink.commit(1).unwrap();
    assert_eq!(std::fs::read(root.join("a/b")).unwrap(), b"hello!!!");
    assert!(!root.join("a/b.ava-part").exists());
}

/// SPEC.md §11.5: `Resume` for a parked job of this peer answers its `JobMap` (the durable
/// file is done) and the job continues; a Resume for an unknown job, or with a manifest hash
/// that is not the stored one, answers `JobMap{status = ERR_UNKNOWN_JOB}` instead of silence.
#[tokio::test(flavor = "multi_thread")]
async fn resume_answers_a_job_map_for_a_parked_job_else_unknown_job() {
    let d = common::temp_dir("rr-resume-frame");
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let job = [0x31; 16];
    let file = |p: &str| Entry {
        kind: gen::ENTRY_FILE,
        mode: 0o644,
        size: 4,
        mtime: 0,
        path: p.into(),
        root: None,
    };
    let m = Manifest {
        entries: vec![file("a"), file("b")],
    };
    {
        let s = connect(
            &addr.to_string(),
            id.clone(),
            peers.clone(),
            "client",
            common::fast(),
        )
        .await
        .unwrap();
        let mut link = s.job(job);
        open_and_map(&mut link, job, "in", &m).await;
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle_body(job, 0, b"one!"))
            .await
            .unwrap();
        loop {
            if next_control(&mut link).await.ty == Durable::TYPE {
                break;
            }
        }
        drop(s); // parked: file 1 never arrived
    }
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    // A job nobody knows.
    let mut link = s.job([0x99; 16]);
    link.control
        .send(&gen::Resume {
            job_id: [0x99; 16],
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!(map.status, gen::ERR_UNKNOWN_JOB);
    drop(link);
    // The parked job, with a manifest hash that is not the stored one.
    let mut link = s.job(job);
    link.control
        .send(&gen::Resume {
            job_id: job,
            manifest_hash: [7; 32],
        })
        .await
        .unwrap();
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!(map.status, gen::ERR_UNKNOWN_JOB);
    drop(link);
    // The parked job, with its own hash: the map, with the durable file done.
    let mut link = s.job(job);
    link.control
        .send(&gen::Resume {
            job_id: job,
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    let credit: Credit = next_control(&mut link).await.decode().unwrap();
    assert_eq!(credit.bytes, 64 << 20, "the grant is re-sent as Credit");
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!(map.status, gen::STATUS_OK);
    assert_eq!(map.done, vec![gen::FileRun { first: 0, count: 1 }]);
}

/// A forward-only source over in-memory entries, in an order unrelated to the manifest's.
struct ReverseSeq {
    entries: Vec<(String, Vec<u8>)>,
}

impl ava1::seq::SeqSource for ReverseSeq {
    fn pass(
        &self,
        restart: ava1::seq::Restart,
        want: &mut dyn FnMut(&str, u64) -> ava1::seq::Keep,
        sink: &mut dyn ava1::seq::EntrySink,
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<()> {
        for (p, d) in self.entries.iter().skip(restart.0 as usize) {
            if want(p, d.len() as u64) == ava1::seq::Keep::Skip {
                continue;
            }
            sink.begin(p)?;
            for c in d.chunks(65536) {
                sink.data(c)?;
            }
            sink.end()?;
        }
        Ok(())
    }
    fn restart_for(&self, _id: u32) -> ava1::seq::Restart {
        ava1::seq::Restart::START
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sequential_source_uploads_into_a_folder_host() {
    let d = common::temp_dir("rr-seq");
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let pat = |n: usize, k: u8| -> Vec<u8> { (0..n).map(|i| (i as u8) ^ k).collect() };
    // Decode order is the reverse of the sorted manifest; one file is large (>1 group).
    let mut entries = vec![
        ("z/small".to_string(), pat(100, 1)),
        ("m/big.bin".to_string(), pat(3 * 1024 * 1024 + 17, 2)),
        ("a".to_string(), pat(0, 3)),
        ("a2".to_string(), pat(5000, 4)),
    ];
    let mut sorted = entries.clone();
    sorted.sort();
    let m = Manifest {
        entries: sorted
            .iter()
            .map(|(p, b)| Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: b.len() as u64,
                mtime: 0,
                path: p.clone(),
                root: None,
            })
            .collect(),
    };
    let mut o = SendOptions::upload("in");
    o.seq = Some(Arc::new(ReverseSeq {
        entries: entries.clone(),
    }));
    let empty = common::temp_dir("rr-seq-src");
    let mut link = s.job([2; 16]);
    let r = send_job(&mut link, Arc::new(m), Arc::new(LocalSource::new(empty)), o)
        .await
        .unwrap();
    assert_eq!(r.status, 0);
    entries.sort();
    for (p, b) in &entries {
        assert_eq!(
            &std::fs::read(d.join("share/in").join(p)).unwrap(),
            b,
            "{p}"
        );
    }
}

// ---- review 006 #2: the receiver's progress watchdog ---------------------------------

/// A sink that makes every sync batch slow (a slow drive), over a real folder sink.
struct SlowSyncSink {
    inner: LocalSink,
    delay: Duration,
}

impl Sink for SlowSyncSink {
    fn prepare(&self, m: &Manifest) -> std::io::Result<()> {
        self.inner.prepare(m)
    }
    fn write_at(&self, id: u32, off: u64, d: &[u8]) -> std::io::Result<()> {
        self.inner.write_at(id, off, d)
    }
    fn write_whole(&self, id: u32, d: &[u8]) -> std::io::Result<()> {
        self.inner.write_whole(id, d)
    }
    fn sync(&self, ids: &[u32]) -> std::io::Result<()> {
        std::thread::sleep(self.delay);
        self.inner.sync(ids)
    }
    fn read_at(&self, id: u32, off: u64, b: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read_at(id, off, b)
    }
    fn commit(&self, id: u32) -> std::io::Result<()> {
        self.inner.commit(id)
    }
    fn finish(&self) -> std::io::Result<()> {
        self.inner.finish()
    }
}

/// Receives with a short progress deadline and reports how the job ended.
struct DeadlineHost {
    root: std::path::PathBuf,
    jobs: std::path::PathBuf,
    deadline: Duration,
    sync_delay: Option<Duration>,
    ended: tokio::sync::mpsc::UnboundedSender<Result<u32, String>>,
}

impl JobHost for DeadlineHost {
    fn accept(&self, mut link: JobLink, first: Frame, _peer: [u8; 32]) {
        let (root, jobs, deadline, delay) = (
            self.root.clone(),
            self.jobs.clone(),
            self.deadline,
            self.sync_delay,
        );
        let ended = self.ended.clone();
        tokio::spawn(async move {
            let Ok(open) = first.decode::<JobOpen>() else {
                return;
            };
            let inner = LocalSink::new(root.join(&open.root), false);
            let sink: Arc<dyn Sink> = match delay {
                Some(delay) => Arc::new(SlowSyncSink { inner, delay }),
                None => Arc::new(inner),
            };
            let o = RecvOptions {
                credit: 1 << 20,
                flags: open.flags,
                jobs_dir: jobs,
                ordered: false,
                progress: Arc::default(),
                cancel: Arc::default(),
                progress_deadline: Some(deadline),
            };
            let r = receive_job(&mut link, open, sink, o).await;
            let _ = ended.send(r.map(|r| r.files).map_err(|e| e.to_string()));
        });
    }
}

fn small_files(n: u32) -> Manifest {
    Manifest {
        entries: (0..n)
            .map(|i| Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 4,
                mtime: 0,
                path: format!("f{i}.bin"),
                root: None,
            })
            .collect(),
    }
}

async fn deadline_job(
    tag: &str,
    job: u8,
    files: u32,
    deadline: Duration,
    sync_delay: Option<Duration>,
) -> (
    JobLink,
    ava1::session::Session,
    tokio::sync::mpsc::UnboundedReceiver<Result<u32, String>>,
    std::path::PathBuf,
    u32,
) {
    let d = common::temp_dir(tag);
    let (ended, rx) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(DeadlineHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        deadline,
        sync_delay,
        ended,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([job; 16]);
    let m = small_files(files);
    let (ack, _map) = open_and_map(&mut link, [job; 16], "in", &m).await;
    assert_eq!(ack.credit, 1 << 20);
    (link, s, rx, d, files)
}

/// A sender that heartbeats (the session pings on its own) but never sends file data: the
/// receiver ends the job with ERR_STALLED instead of waiting on the byte-level watchdog
/// forever, and says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_sender_that_only_pings_is_cancelled_with_err_stalled() {
    let (mut link, _s, mut ended, _d, _n) =
        deadline_job("rr-stall", 0x91, 2, Duration::from_millis(800), None).await;
    let _lane = link.opener().unwrap().open().await.unwrap();
    let t0 = std::time::Instant::now();
    let cancel = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let f = next_control(&mut link).await;
            if f.ty == gen::JobCancel::TYPE {
                break f.decode::<gen::JobCancel>().unwrap();
            }
        }
    })
    .await
    .expect("the receiver cancelled the stalled job within 10 s");
    assert_eq!(cancel.reason, gen::ERR_STALLED);
    assert!(
        t0.elapsed() >= Duration::from_millis(700),
        "not before the deadline: {:?}",
        t0.elapsed()
    );
    let why = tokio::time::timeout(Duration::from_secs(5), ended.recv())
        .await
        .expect("the receiver returned")
        .expect("a result")
        .expect_err("a stalled job is an error");
    assert!(why.contains("stalled"), "{why}");
}

/// A slow-but-moving sender is never cut: one small file every half deadline keeps the job
/// alive across several deadlines and it completes.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_but_moving_sender_is_not_cut() {
    let (link, _s, mut ended, d, n) =
        deadline_job("rr-slow-moving", 0x92, 4, Duration::from_millis(800), None).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    for i in 0..n {
        tokio::time::sleep(Duration::from_millis(400)).await;
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, i + 1, bundle_body([0x92; 16], i, b"data"))
            .await
            .unwrap();
    }
    let files = tokio::time::timeout(Duration::from_secs(20), ended.recv())
        .await
        .expect("the job ended")
        .expect("a result")
        .expect("it completed, was not cut");
    assert_eq!(files, n);
    assert_eq!(
        std::fs::read(d.join("share/in/f3.bin")).unwrap(),
        b"data".to_vec()
    );
}

/// Disk work in flight is progress too: a sync batch that outlasts the deadline (a slow
/// drive) must not read as a stalled sender.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_disk_batch_is_not_a_stall() {
    let (mut link, _s, mut ended, _d, _n) = deadline_job(
        "rr-slow-disk",
        0x93,
        2,
        Duration::from_millis(800),
        Some(Duration::from_millis(2000)),
    )
    .await;
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle_body([0x93; 16], 0, b"one!"))
        .await
        .unwrap();
    // Wait for f0's Durable (the 2 s batch), then send the last file well inside a deadline.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let f = next_control(&mut link).await;
            if f.ty == Durable::TYPE {
                break;
            }
        }
    })
    .await
    .expect("the slow batch finished");
    tokio::time::sleep(Duration::from_millis(300)).await;
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 2, bundle_body([0x93; 16], 1, b"two!"))
        .await
        .unwrap();
    let files = tokio::time::timeout(Duration::from_secs(30), ended.recv())
        .await
        .expect("the job ended")
        .expect("a result")
        .expect("a slow disk was not cut");
    assert_eq!(files, 2);
}

/// Review 006 #4 (checklist T): job admission is bounded. A paired peer that opens more jobs
/// than the host admits at once is refused `ERR_BUSY` for the extra ones; the session and the
/// admitted jobs go on, and a slot freed by a finished job admits the next open.
#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_job_opens_is_bounded_with_err_busy() {
    let d = common::temp_dir("rr-admission");
    let host = Arc::new(CreditHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        credit: 1 << 20,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let cap = ava1::server::MAX_JOBS_PER_SESSION;
    let mut links = Vec::new();
    for n in 0..cap {
        let job = [n as u8 + 1; 16];
        let mut link = s.job(job);
        link.control
            .send(&JobOpen {
                job_id: job,
                kind: gen::JOB_UPLOAD,
                policy: 0,
                flags: 0,
                root: format!("in{n}"),
                src: None,
                credit: None,
            })
            .await
            .unwrap();
        let ack: JobOpenAck = next_control(&mut link).await.decode().unwrap();
        assert_eq!(ack.status, 0, "job {n} is within the cap");
        links.push(link);
    }
    // One more is refused, not queued: BUSY, and the session stays.
    let over = [0xee; 16];
    let mut extra = s.job(over);
    extra
        .control
        .send(&JobOpen {
            job_id: over,
            kind: gen::JOB_UPLOAD,
            policy: 0,
            flags: 0,
            root: "over".into(),
            src: None,
            credit: None,
        })
        .await
        .unwrap();
    let ack: JobOpenAck = next_control(&mut extra).await.decode().unwrap();
    assert_eq!(ack.status, gen::ERR_BUSY);
    assert!(!s.is_closed());
    // An admitted job still works: finishing one (an empty manifest) frees a slot for the next
    // open, which the host admits once the finished job has unregistered.
    let first = [1u8; 16];
    let m = Manifest::default();
    let l0 = &mut links[0];
    for p in m.pages(first) {
        l0.control.send(&p).await.unwrap();
    }
    l0.control
        .send(&ManifestEnd {
            job_id: first,
            files: 0,
            bytes: 0,
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    loop {
        let f = next_control(l0).await;
        if f.ty == JobDone::TYPE {
            break;
        }
    }
    drop(extra);
    let mut again = s.job(over);
    again
        .control
        .send(&JobOpen {
            job_id: over,
            kind: gen::JOB_UPLOAD,
            policy: 0,
            flags: 0,
            root: "over".into(),
            src: None,
            credit: None,
        })
        .await
        .unwrap();
    let mut ack: JobOpenAck = next_control(&mut again).await.decode().unwrap();
    // The finished job unregisters a moment after its JobDone: BUSY until then.
    for _ in 0..40 {
        if ack.status == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        again
            .control
            .send(&JobOpen {
                job_id: over,
                kind: gen::JOB_UPLOAD,
                policy: 0,
                flags: 0,
                root: "over".into(),
                src: None,
                credit: None,
            })
            .await
            .unwrap();
        ack = next_control(&mut again).await.decode().unwrap();
    }
    assert_eq!(ack.status, 0, "a freed slot admits the next open");
}

/// Review 006 follow-up: a resume whose journal holds only finished files waits the longer
/// resume deadline (25 x), so a sender that spends several fresh deadlines hashing or skipping
/// what is durable, sending no frame, is not cut; a fresh job with the same silence is.
#[tokio::test(flavor = "multi_thread")]
async fn a_done_only_resume_outlives_the_fresh_deadline() {
    let d = common::temp_dir("rr-resume-silent");
    let (ended, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(DeadlineHost {
        root: d.join("share"),
        jobs: d.join("hjobs"),
        deadline: Duration::from_millis(400),
        sync_delay: None,
        ended,
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let job = [0x94u8; 16];
    let m = small_files(3);
    // First run: file 0 lands and is durable, then the sender goes away.
    {
        let s = connect(
            &addr.to_string(),
            id.clone(),
            peers.clone(),
            "c",
            common::fast(),
        )
        .await
        .unwrap();
        let mut link = s.job(job);
        let _ = open_and_map(&mut link, job, "in", &m).await;
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle_body(job, 0, b"zero"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if next_control(&mut link).await.ty == Durable::TYPE {
                    break;
                }
            }
        })
        .await
        .expect("file 0 became durable");
    }
    // The receiver of the first run ends with the session.
    let _ = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    // Second run: done-only resume. The sender is silent for 3 s (7 fresh deadlines), then sends.
    let s = connect(&addr.to_string(), id, peers, "c", common::fast())
        .await
        .unwrap();
    let mut link = s.job(job);
    let (_, need) = open_and_map(&mut link, job, "in", &m).await;
    assert!(need.done.contains(&0), "the resume sees file 0 done");
    let lane = link.opener().unwrap().open().await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    for i in 1..3u32 {
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, i, bundle_body(job, i, b"data"))
            .await
            .unwrap();
    }
    let files = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("the resumed job ended")
        .expect("a result")
        .expect("it completed, was not cut during the silence");
    assert_eq!(files, 3);
}

/// Final review engine #3: zero-byte files in an ordered download verify as the empty file
/// without a read-back, so no sink loops on `FileRetry`.
async fn ordered_with_empty_files(tag: &str, job: u8, preexisting_root: bool) {
    let d = common::temp_dir(tag);
    let root = d.join("share/out");
    for i in 0..30usize {
        let p = root.join(format!("a{}/f{i}", i % 3));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let n = if i % 3 == 0 { 0 } else { i * 40 };
        std::fs::write(p, (0..n).map(|k| (k + i) as u8).collect::<Vec<_>>()).unwrap();
    }
    std::fs::write(root.join("big.bin"), vec![7u8; (2 << 20) + 5]).unwrap();
    std::fs::write(root.join("empty.bin"), b"").unwrap();
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host)).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([job; 16]);
    if preexisting_root {
        std::fs::create_dir_all(d.join("got")).unwrap(); // not staged: files land in place
    }
    let sink = Arc::new(LocalSink::new(d.join("got"), false));
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        download_job(
            &mut link,
            "out",
            gen::JF_ORDERED,
            sink,
            opts(&d.join("jobs"), true),
        ),
    )
    .await
    .expect("an ordered download with empty files must not loop on FileRetry")
    .unwrap();
    assert_eq!(r.files, 32);
    same(&d.join("share/out"), &d.join("got"));
    assert!(d.join("got/empty.bin").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordered_download_with_empty_files_finishes_staged() {
    ordered_with_empty_files("rr-empty-staged", 11, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordered_download_with_empty_files_finishes_in_place() {
    ordered_with_empty_files("rr-empty-inplace", 12, true).await;
}
