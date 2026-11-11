//! Tar and `.zst` uploads over AVA1, and zip entries compressed with zstd: the tar source
//! (`TarSource`) against the Rust folder host on 127.0.0.1, as `sevenz.rs` does for 7z.
//! Archives are written here (the `tar` crate, zstd's encoder, flate2); every wait is bounded.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::manifest::Manifest;
use ava1::peers::PeerStore;
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ps5upload_ava1::gen;
use ps5upload_ava1::tar_source::TarSource;
use ps5upload_ava1::upload::{self, UploadFailure};
use ps5upload_ava1::Pool;
use ps5upload_core::tar_archive::TarWatch;
use ps5upload_core::transfer::TransferConfig;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5t-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(n);
    v
}

#[derive(Clone, Copy, PartialEq)]
enum Z {
    None,
    Zstd,
    Gzip,
}

fn header(kind: tar::EntryType, size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(1_700_000_000);
    h
}

/// `files` as a tar, compressed by `z`, plus a symbolic link the upload leaves out.
fn tar_of(files: &[(String, Vec<u8>)], z: Z) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (n, d) in files {
        let mut h = header(tar::EntryType::Regular, d.len() as u64);
        b.append_data(&mut h, n, &d[..]).unwrap();
    }
    let mut l = header(tar::EntryType::Symlink, 0);
    b.append_link(&mut l, "link", "nowhere").unwrap();
    let raw = b.into_inner().unwrap();
    match z {
        Z::None => raw,
        Z::Zstd => zstd::encode_all(&raw[..], 1).unwrap(),
        Z::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(&raw).unwrap();
            e.finish().unwrap()
        }
    }
}

/// `n` files of about `size` bytes in a few folders, one long name among them.
fn files(n: usize, size: usize) -> Vec<(String, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let name = if i == 3 {
                format!("game/{}/f{i}", "long".repeat(40))
            } else {
                format!("game/d{}/f{i}", i % 3)
            };
            (name, noise(i as u64 + 1, size + i))
        })
        .collect()
}

fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

fn same(want: &[(String, Vec<u8>)], got: &BTreeMap<String, Vec<u8>>) {
    let want: BTreeMap<String, Vec<u8>> = want.iter().cloned().collect();
    assert_eq!(
        want.keys().collect::<Vec<_>>(),
        got.keys().collect::<Vec<_>>(),
        "file sets differ"
    );
    for (k, v) in &want {
        assert!(&got[k] == v, "{k}: bytes differ");
    }
}

async fn within<T>(secs: u64, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("timed out")
}

fn node_info_rpc() -> ava1::server::RpcHandler {
    Box::new(|method, _| {
        if method == gen::METHOD_NODE_INFO {
            let info = gen::NodeInfo {
                version: "test".into(),
                platform: "rust".into(),
                name: "host".into(),
                firmware: None,
            };
            RpcReply {
                status: gen::STATUS_OK,
                body: info.to_bytes().unwrap(),
            }
        } else {
            RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: Vec::new(),
            }
        }
    })
}

async fn host(dir: &Path) -> (String, PathBuf) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(
        Identity::load_or_create(&dir.join("host-id")).unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(Arc::new(FolderHost {
        root: dir.join("share"),
        jobs_dir: dir.join("hjobs"),
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    (addr, ava)
}

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_total = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

fn failure(e: &anyhow::Error) -> &UploadFailure {
    e.downcast_ref::<UploadFailure>()
        .unwrap_or_else(|| panic!("not a typed failure: {e:#}"))
}

async fn upload(
    d: &Path,
    arc: &Path,
    dest: &str,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (addr, ava) = host(d).await;
    let pool = Pool::new(ava).with_addr(addr);
    let arc = arc.to_owned();
    let dest = dest.to_owned();
    within(
        120,
        tokio::task::spawn_blocking(move || {
            upload::upload_tar_in(&pool, &cfg(), [9; 16], &dest, &arc)
        }),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_and_compressed_tars_and_a_lone_zst_land_unpacked() {
    let d = temp_dir("upload");
    let want = files(40, 20_000);
    for (z, name) in [
        (Z::None, "a.tar"),
        (Z::Zstd, "a.tar.zst"),
        (Z::Gzip, "a.tgz"),
    ] {
        let arc = d.join(name);
        std::fs::write(&arc, tar_of(&want, z)).unwrap();
        let dest = format!("out-{name}");
        let (addr, ava) = host(&d).await;
        let pool = Pool::new(ava).with_addr(addr);
        let c = cfg();
        let (fin, total) = (
            c.progress_bytes_finalized.clone().unwrap(),
            c.progress_total.clone().unwrap(),
        );
        let (arc2, dest2) = (arc.clone(), dest.clone());
        let r = within(
            120,
            tokio::task::spawn_blocking(move || {
                upload::upload_tar_in(&pool, &c, [5; 16], &dest2, &arc2)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
        assert_eq!(ack["files"], want.len(), "{name}");
        let bytes: u64 = want.iter().map(|(_, v)| v.len() as u64).sum();
        same(&want, &read_tree(&d.join("share").join(&dest)));
        assert_eq!(fin.load(Ordering::Relaxed), bytes, "{name}");
        // The job's total is the listing's, written before the first byte went out.
        assert_eq!(total.load(Ordering::Relaxed), bytes, "{name}");
        assert_eq!(r.tx_id_hex, "05".repeat(16));
        let (_, src) = TarSource::open(&arc, &[], TarWatch::default()).unwrap();
        assert_eq!(src.sequential(), z != Z::None, "{name}");
        assert_eq!(src.left_out(), (1, 0), "{name}: the link is left out");
    }
    // A lone .zst is its one file, named without .zst.
    let data = noise(77, 300_000);
    let arc = d.join("disc.exfat.zst");
    std::fs::write(&arc, zstd::encode_all(&data[..], 1).unwrap()).unwrap();
    upload(&d, &arc, "out-zst").await.unwrap();
    let got = read_tree(&d.join("share/out-zst"));
    assert_eq!(got.keys().collect::<Vec<_>>(), vec!["disc.exfat"]);
    assert!(got["disc.exfat"] == data);
}

#[derive(Default)]
struct Collect {
    files: BTreeMap<String, Vec<u8>>,
    cur: Option<String>,
}
impl EntrySink for Collect {
    fn begin(&mut self, path: &str) -> io::Result<()> {
        self.cur = Some(path.into());
        self.files.insert(path.into(), Vec::new());
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.files
            .get_mut(self.cur.as_ref().unwrap())
            .unwrap()
            .extend_from_slice(bytes);
        Ok(())
    }
    fn end(&mut self) -> io::Result<()> {
        self.cur = None;
        Ok(())
    }
}

/// A resume pass over a compressed tar: from the start, the done files decoded and dropped,
/// and exactly what `bytes_to_skip` said.
#[test]
fn a_compressed_resume_pass_skips_the_done_files_and_predicts_it() {
    let d = temp_dir("seqpass");
    let want = files(12, 10_000);
    let arc = d.join("p.tar.gz");
    std::fs::write(&arc, tar_of(&want, Z::Gzip)).unwrap();
    let (m, src): (Manifest, TarSource) = TarSource::open(&arc, &[], TarWatch::default()).unwrap();
    assert!(m
        .entries
        .iter()
        .any(|e| e.kind == gen::ENTRY_DIR && e.path == "game/d0"));
    let done: Vec<&str> = want[..7].iter().map(|(n, _)| n.as_str()).collect();
    let mut want_fn = |p: &str, _| {
        if done.contains(&p) {
            Keep::Skip
        } else {
            Keep::All
        }
    };
    assert_eq!(src.restart_for(5), Restart::START);
    let predicted = SeqSource::bytes_to_skip(&src, Restart::START, &mut want_fn);
    let mut sink = Collect::default();
    src.pass(
        Restart::START,
        &mut want_fn,
        &mut sink,
        &AtomicBool::new(false),
    )
    .unwrap();
    let skipped: u64 = want[..7].iter().map(|(_, v)| v.len() as u64).sum();
    assert_eq!(src.bytes_skipped(), skipped);
    assert_eq!(predicted, Some(skipped));
    assert_eq!(sink.files.len(), 5);
    for (n, v) in &want[7..] {
        assert!(&sink.files[n] == v, "{n}");
    }
    // Excludes leave files out of the manifest, and the pass reads past them.
    let (m, _) = TarSource::open(&arc, &["d1".to_string()], TarWatch::default()).unwrap();
    assert!(!m.entries.iter().any(|e| e.path.starts_with("game/d1/")));
}

/// Uploads `arc` through a throttled proxy and cancels once `fin_bytes` are durable;
/// returns the durable byte count at the cancel.
async fn partial_upload(d: &Path, arc: &Path, job: [u8; 16], fin_bytes: u64) -> u64 {
    let (addr, ava) = host(d).await;
    let proxy = Arc::new(
        ChaosProxy::start(
            addr.parse().unwrap(),
            ChaosConfig {
                bytes_per_sec: Some(512 << 10),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let pool = Pool::new(ava).with_addr(proxy.addr.to_string());
    let c = cfg();
    let fin = c.progress_bytes_finalized.clone().unwrap();
    let cancel = c.cancel.clone().unwrap();
    let watcher = {
        let fin = fin.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            while fin.load(Ordering::Relaxed) < fin_bytes {
                assert!(
                    t0.elapsed() < Duration::from_secs(60),
                    "never reached the cut"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            cancel.store(true, Ordering::Relaxed);
        })
    };
    let arc = arc.to_owned();
    let e = within(
        90,
        tokio::task::spawn_blocking(move || upload::upload_tar_in(&pool, &c, job, "out", &arc)),
    )
    .await
    .unwrap()
    .unwrap_err();
    watcher.join().unwrap();
    assert!(e.to_string().contains("transfer_cancelled"), "{e:#}");
    drop(proxy);
    fin.load(Ordering::Relaxed)
}

fn wire_bytes(r: &ps5upload_core::transfer::TransferResult, progress: &AtomicU64) -> u64 {
    let skipped = serde_json::from_str::<serde_json::Value>(&r.commit_ack_body)
        .ok()
        .and_then(|v| v["skipped_bytes"].as_u64())
        .unwrap_or(0);
    progress.load(Ordering::Relaxed).saturating_sub(skipped)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_upload_resumes_and_sends_only_what_is_missing() {
    for (z, name) in [(Z::Zstd, "r.tar.zst"), (Z::None, "r.tar")] {
        let d = temp_dir(&format!("resume-{}", z as u8));
        // Far more than the cut, so the cancel lands long before the end even when the durable
        // count trails the wire (CI finished a 2.6 MB upload before 1 MiB showed as durable).
        let want = files(240, 64 * 1024 - 20);
        let arc = d.join(name);
        std::fs::write(&arc, tar_of(&want, z)).unwrap();
        let total: u64 = want.iter().map(|(_, v)| v.len() as u64).sum();
        let fin1 = partial_upload(&d, &arc, [3; 16], 1 << 20).await;
        assert!(fin1 >= 1 << 20 && fin1 < total, "{name}: {fin1} of {total}");
        let (addr, ava) = host(&d).await;
        let pool = Pool::new(ava).with_addr(addr);
        let c = cfg();
        let sent = c.progress_bytes.clone().unwrap();
        let arc2 = arc.clone();
        let r = within(
            120,
            tokio::task::spawn_blocking(move || {
                upload::upload_tar_in(&pool, &c, [3; 16], "out", &arc2)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        same(&want, &read_tree(&d.join("share/out")));
        let on_wire = wire_bytes(&r, &sent);
        assert!(
            on_wire <= total - fin1,
            "{name}: sent {on_wire} but only {} were missing",
            total - fin1
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn damaged_unsafe_and_too_big_archives_fail_with_their_reasons() {
    let d = temp_dir("fail");
    // A flipped byte in the middle of a gzip stream: its CRC (or the inflate) fails.
    let want = files(10, 50_000);
    let mut bytes = tar_of(&want, Z::Gzip);
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x55;
    let arc = d.join("bad.tar.gz");
    std::fs::write(&arc, &bytes).unwrap();
    let e = upload(&d, &arc, "o1").await.unwrap_err();
    assert_eq!(failure(&e).reason, "ava1_tar_corrupt", "{e:#}");

    // `..` in a name.
    let mut h = header(tar::EntryType::Regular, 1);
    h.as_old_mut().name[..9].copy_from_slice(b"../escape");
    h.set_cksum();
    let mut b = tar::Builder::new(Vec::new());
    b.append(&h, &b"x"[..]).unwrap();
    let arc = d.join("evil.tar");
    std::fs::write(&arc, b.into_inner().unwrap()).unwrap();
    let e = upload(&d, &arc, "o2").await.unwrap_err();
    assert_eq!(failure(&e).reason, "ava1_tar_unsafe_path", "{e:#}");
    assert!(!d.join("share/escape").exists());

    // A zstd frame that asks for a 4 GiB window (window log 32): over every cap.
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 22 << 3];
    f.extend_from_slice(&[9, 0, 0, b'x']);
    let arc = d.join("huge.tar.zst");
    std::fs::write(&arc, &f).unwrap();
    let e = upload(&d, &arc, "o3").await.unwrap_err();
    let fail = failure(&e);
    assert_eq!(fail.reason, "ava1_tar_memory", "{e:#}");
    assert!(
        fail.detail.contains("4 GiB") && fail.detail.contains("--long"),
        "{}",
        fail.detail
    );
}

/// A zip whose entries are zstd (method 93), written by hand: the `zip` crate here has no zstd.
fn zstd_zip(files: &[(&str, &[u8])], corrupt_crc: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let comp = zstd::encode_all(*data, 1).unwrap();
        let crc = crc32fast::hash(data) ^ u32::from(corrupt_crc);
        let off = out.len() as u32;
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&63u16.to_le_bytes()); // version needed
            v.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            v.extend_from_slice(&93u16.to_le_bytes()); // zstd
            v.extend_from_slice(&0u16.to_le_bytes()); // time
            v.extend_from_slice(&0x21u16.to_le_bytes()); // date: 1980-01-01
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes()); // extra
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x033Fu16.to_le_bytes()); // made by: unix
        common(&mut central);
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_entries_compressed_with_zstd_upload_and_are_checked() {
    let d = temp_dir("zipzstd");
    let a = noise(1, 400_000);
    let b = noise(2, 1000);
    let arc = d.join("z.zip");
    std::fs::write(
        &arc,
        zstd_zip(&[("g/a.bin", &a), ("g/sub/b.bin", &b)], false),
    )
    .unwrap();
    let (addr, ava) = host(&d).await;
    let pool = Pool::new(ava).with_addr(addr);
    let arc2 = arc.clone();
    within(
        120,
        tokio::task::spawn_blocking(move || {
            upload::upload_zip_in(&pool, &cfg(), [1; 16], "zo", &arc2)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let got = read_tree(&d.join("share/zo"));
    assert!(got["g/a.bin"] == a && got["g/sub/b.bin"] == b);

    // A wrong CRC-32 is a corrupt archive, as for deflate.
    let arc = d.join("bad.zip");
    std::fs::write(&arc, zstd_zip(&[("a.bin", &a)], true)).unwrap();
    let (addr, ava) = host(&d).await;
    let pool = Pool::new(ava).with_addr(addr);
    let e = within(
        120,
        tokio::task::spawn_blocking(move || {
            upload::upload_zip_in(&pool, &cfg(), [2; 16], "zb", &arc)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(failure(&e).reason, "ava1_zip_corrupt", "{e:#}");
}
