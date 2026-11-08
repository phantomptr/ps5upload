//! Convert to the console against AVA1's loopback receiver (a folder host on 127.0.0.1): every
//! format arrives byte for byte as a local build of the same game, a cancel commits nothing, a
//! dropped connection and a second try both go on and give the same bytes.

use super::*;
use crate::image_build::DEFAULT_VERIFY_CHUNK;
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;
use ps5upload_fpkg::source::FolderSource;
use ps5upload_fpkg::ReadSeek;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

/// The console's files as the loopback receiver holds them.
pub(crate) struct ShareFiles(pub PathBuf);

impl RemoteFiles for ShareFiles {
    fn open(&self, path: &str) -> io::Result<Box<dyn ReadSeek>> {
        Ok(Box::new(std::fs::File::open(self.0.join(path))?))
    }
    fn stat(&self, path: &str) -> io::Result<(u64, bool)> {
        let m = std::fs::metadata(self.0.join(path))?;
        Ok((m.len(), m.is_dir()))
    }
    fn list(&self, _dir: &str) -> io::Result<Vec<(String, bool, u64)>> {
        Ok(Vec::new())
    }
    fn label(&self) -> String {
        "the test receiver".into()
    }
}

/// A folder host on 127.0.0.1 that trusts this engine's key, optionally behind a chaos proxy.
pub(crate) struct Loopback {
    pub dir: PathBuf,
    pub pool: Pool,
    pub proxy: Option<Arc<ava1_chaos::ChaosProxy>>,
}

impl Loopback {
    pub(crate) async fn start(tag: &str, chaos: Option<ava1_chaos::ChaosConfig>) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ps5upload-tocon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ava = dir.join("engine");
        let key = Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        std::fs::create_dir_all(dir.join("host/share")).unwrap();
        let mut peers = PeerStore::in_memory();
        peers.add(key, "engine").unwrap();
        let rpc: ava1::server::RpcHandler = Box::new(|_, _| ava1::session::RpcReply {
            status: ps5upload_ava1::gen::ERR_UNKNOWN_METHOD,
            body: Vec::new(),
        });
        let ctx = ServerCtx::new(
            Identity::load_or_create(&dir.join("host-id")).unwrap(),
            "host",
            peers,
            rpc,
        )
        .with_jobs(Arc::new(FolderHost {
            root: dir.join("host/share"),
            jobs_dir: dir.join("host/jobs"),
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        let proxy = match chaos {
            Some(c) => Some(Arc::new(
                ava1_chaos::ChaosProxy::start(addr, c).await.unwrap(),
            )),
            None => None,
        };
        let via = proxy
            .as_ref()
            .map_or(addr.to_string(), |p| p.addr.to_string());
        let pool = Pool::new(ava)
            .with_addr(via)
            .with_room_probe(Arc::new(|_, _| None));
        Self { dir, pool, proxy }
    }

    pub(crate) fn share(&self) -> PathBuf {
        self.dir.join("host/share")
    }

    /// Runs `f` with a target for `path` on the receiver.
    pub(crate) fn with_target<R>(
        &self,
        cfg: &TransferConfig,
        path: &str,
        f: impl FnOnce(&ConsoleTarget) -> R,
    ) -> R {
        let send = |m: Manifest, src: Arc<dyn Source>, seq: Arc<dyn SeqSource>, job: [u8; 16]| {
            let mut opts = ava1::send::SendOptions::upload(path);
            opts.flags = ps5upload_ava1::gen::JF_SINGLE_FILE;
            ps5upload_ava1::upload::upload_with_seq_in(
                &self.pool,
                "127.0.0.1",
                job,
                m,
                src,
                Some(seq),
                opts,
                cfg,
            )
        };
        let share = self.share();
        let remove = move |p: &str| -> anyhow::Result<()> {
            std::fs::remove_file(share.join(p))?;
            Ok(())
        };
        let target = ConsoleTarget {
            console: "127.0.0.1".into(),
            path: path.into(),
            send: &send,
            files: Arc::new(ShareFiles(self.share())),
            remove: &remove,
            room: None,
        };
        f(&target)
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

/// A small game with `big` bytes of data that do not repeat, a zero-filled file, an empty file
/// and an empty folder.
fn game(root: &Path, big: usize) -> PathBuf {
    let game = root.join("PPSA99999-app");
    let _ = std::fs::remove_dir_all(&game);
    std::fs::create_dir_all(game.join("sce_sys")).unwrap();
    std::fs::create_dir_all(game.join("data/sub")).unwrap();
    std::fs::create_dir_all(game.join("empty")).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let data: Vec<u8> = (0..big)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(game.join("eboot.bin"), &data[..data.len().min(300_000)]).unwrap();
    std::fs::write(game.join("data/sub/big.bin"), &data).unwrap();
    std::fs::write(
        game.join("sce_sys/param.json"),
        b"{\"titleId\":\"PPSA99999\"}",
    )
    .unwrap();
    std::fs::write(game.join("data/sub/a.bin"), b"").unwrap();
    std::fs::write(game.join("data/zeros.bin"), vec![0u8; 200_000]).unwrap();
    game
}

const TIME: i64 = 1_700_000_000;

/// The same game built on this computer, as Convert writes it today.
fn local_build(src: &Path, format: ImageFormat, out: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(out);
    let mut tree = FolderSource::open(src).unwrap();
    super::super::build_any(
        format,
        None,
        Level::default(),
        Some(TIME),
        &mut tree,
        out,
        VerifyMode::Full,
        DEFAULT_VERIFY_CHUNK,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
        None,
    )
    .unwrap_or_else(|e| panic!("local {format:?}: {e}"));
    std::fs::read(out).unwrap()
}

fn stream(
    lb: &Loopback,
    cfg: &TransferConfig,
    src: &Path,
    format: ImageFormat,
    path: &str,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> anyhow::Result<Streamed> {
    let cancel = cfg.cancel.clone().unwrap();
    let mut tree = FolderSource::open(src).unwrap();
    lb.with_target(cfg, path, |t| {
        stream_to_console(
            format,
            Some(TIME),
            "",
            &mut tree,
            t,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &cancel,
            stage,
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn every_format_arrives_as_the_local_build_would_write_it() {
    let lb = Loopback::start("formats", None).await;
    let src = game(&lb.dir, 3 << 20);
    for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
        let want = local_build(
            &src,
            format,
            &lb.dir.join(format!("local.{}", format.extension())),
        );
        let path = format!("dst/G.{}", format.extension());
        let mut stages: Vec<String> = Vec::new();
        let (lb2, src2) = (&lb, src.clone());
        let r = tokio::task::block_in_place(|| {
            stream(lb2, &cfg(), &src2, format, &path, &mut |s, _, _| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            })
        })
        .unwrap_or_else(|e| panic!("{format:?}: {e:#}"));
        assert_eq!(stages, ["plan", "write", "verify"], "{format:?}");
        assert_eq!(r.passes, 1, "{format:?}");
        assert_eq!(r.built.image_bytes, want.len() as u64, "{format:?}");
        let got = std::fs::read(lb.share().join(&path)).unwrap();
        assert!(
            got == want,
            "{format:?}: the console's image differs from the local build"
        );
        assert!(!lb.share().join(format!("{path}.ava-part")).exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_midway_commits_nothing() {
    let lb = Loopback::start("cancel", None).await;
    let src = game(&lb.dir, 8 << 20);
    let c = cfg();
    let cancel = c.cancel.clone().unwrap();
    let path = "dst/G.exfat";
    let e = tokio::task::block_in_place(|| {
        stream(&lb, &c, &src, ImageFormat::Exfat, path, &mut |s, d, t| {
            if s == "write" && t > 0 && d > t / 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
    })
    .err()
    .expect("a cancelled stream fails");
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    assert!(
        !lb.share().join(path).exists(),
        "a cancelled image was committed"
    );
}

/// The connection drops while the image streams: the job reconnects, the writer runs again from
/// the start and the console ends up with the very bytes a local build writes.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_goes_on_and_gives_the_same_bytes() {
    let lb = Loopback::start(
        "drop",
        Some(ava1_chaos::ChaosConfig {
            bytes_per_sec: Some(3 << 20),
            ..Default::default()
        }),
    )
    .await;
    let src = game(&lb.dir, 14 << 20);
    let want = local_build(&src, ImageFormat::Ffpkg, &lb.dir.join("local.ffpkg"));
    let c = cfg();
    let durable = c.progress_bytes_finalized.clone().unwrap();
    let proxy = lb.proxy.clone().unwrap();
    let killer = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while durable.load(Ordering::Relaxed) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "nothing became durable"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        proxy.kill_all();
    });
    let path = "dst/G.ffpkg";
    let r = tokio::task::block_in_place(|| {
        stream(&lb, &c, &src, ImageFormat::Ffpkg, path, &mut |_, _, _| {})
    })
    .unwrap_or_else(|e| panic!("{e:#}"));
    killer.join().unwrap();
    assert!(r.passes >= 2, "the writer ran {} time(s)", r.passes);
    assert!(lb.pool.attempts() >= 2, "the pool never reconnected");
    let got = std::fs::read(lb.share().join(path)).unwrap();
    assert!(
        got == want,
        "the resumed image differs from the local build"
    );
}

/// A first try that stops partway (an engine closed, a cable pulled for good) and the same
/// conversion started again: the same job id finds what the console holds, and the image is
/// whole and right.
#[tokio::test(flavor = "multi_thread")]
async fn starting_again_goes_on_from_what_the_console_holds() {
    let lb = Loopback::start(
        "again",
        Some(ava1_chaos::ChaosConfig {
            bytes_per_sec: Some(3 << 20),
            ..Default::default()
        }),
    )
    .await;
    let src = game(&lb.dir, 14 << 20);
    let want = local_build(&src, ImageFormat::Ffpfs, &lb.dir.join("local.ffpfs"));
    let path = "dst/G.ffpfs";
    let first = cfg();
    let (durable, cancel) = (
        first.progress_bytes_finalized.clone().unwrap(),
        first.cancel.clone().unwrap(),
    );
    let stopper = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while durable.load(Ordering::Relaxed) < 4 << 20 {
            assert!(
                std::time::Instant::now() < deadline,
                "too little became durable"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::Relaxed);
    });
    let e = tokio::task::block_in_place(|| {
        stream(
            &lb,
            &first,
            &src,
            ImageFormat::Ffpfs,
            path,
            &mut |_, _, _| {},
        )
    });
    stopper.join().unwrap();
    assert!(e.is_err(), "the first try was meant to stop partway");
    assert!(!lb.share().join(path).exists());

    let second = cfg();
    let r = tokio::task::block_in_place(|| {
        stream(
            &lb,
            &second,
            &src,
            ImageFormat::Ffpfs,
            path,
            &mut |_, _, _| {},
        )
    })
    .unwrap_or_else(|e| panic!("{e:#}"));
    let got = std::fs::read(lb.share().join(path)).unwrap();
    assert!(got == want, "the image differs from the local build");
    let ack: serde_json::Value = serde_json::from_str(&r.transfer.commit_ack_body).unwrap();
    // What was durable when the first try stopped is what the console kept for the second.
    let resumed = ack["resumed_bytes"].as_u64().unwrap_or(0);
    assert!(
        resumed >= 4 << 20,
        "the second try did not go on from the first: {ack}"
    );
}

#[test]
fn the_pipe_writer_only_moves_forward_and_keeps_to_the_plan() {
    let (tx, rx) = mpsc::sync_channel(64);
    let abort = AtomicBool::new(false);
    let mut w = PipeWriter::new(&tx, &abort, 3 * PIECE as u64);
    w.write_all(b"abc").unwrap();
    assert_eq!(
        w.seek(SeekFrom::Start(PIECE as u64 + 5)).unwrap(),
        PIECE as u64 + 5
    );
    assert_eq!(w.seek(SeekFrom::End(0)).unwrap(), PIECE as u64 + 5);
    assert!(w.seek(SeekFrom::Start(4)).is_err());
    assert!(w.seek(SeekFrom::End(-1)).is_err());
    w.write_all(b"z").unwrap();
    assert!(w.finish().is_err(), "short of the planned size");
    w.seek(SeekFrom::Start(3 * PIECE as u64)).unwrap();
    assert!(w.write_all(b"!").is_err(), "past the planned size");
    w.finish().unwrap();
    drop(w);
    drop(tx);
    let mut all = Vec::new();
    for p in rx {
        if let Piece::Data(b) = p {
            assert!(b.len() <= PIECE);
            all.extend_from_slice(&b);
        }
    }
    assert_eq!(all.len(), 3 * PIECE);
    assert_eq!(&all[..3], b"abc");
    assert_eq!(all[PIECE + 5], b'z');
    assert!(all[3..PIECE + 5].iter().all(|b| *b == 0));
    // A stopped pass ends the writer at its next piece.
    let (tx, rx) = mpsc::sync_channel(1);
    drop(rx);
    let mut w = PipeWriter::new(&tx, &abort, 4 * PIECE as u64);
    assert!(w.write_all(&vec![1u8; 2 * PIECE]).is_err());
}

#[test]
fn not_enough_room_is_refused_with_the_consoles_figures() {
    let room = ps5upload_ava1::space::Room {
        volume: "/data".into(),
        dev: None,
        free_bytes: 1000,
        reserve_bytes: 100,
        allocatable_bytes: 900,
        total_bytes: 10_000,
        image: false,
    };
    assert!(room_check(900, 0, &room).is_ok());
    let r = room_check(1500, 0, &room).unwrap_err();
    assert_eq!((r.need_bytes, r.avail_bytes), (1500, 900));
    // What the console already holds of this image counts.
    assert!(room_check(1500, 700, &room).is_ok());
    let e = no_room(r);
    let f = e
        .downcast_ref::<ps5upload_ava1::upload::UploadFailure>()
        .unwrap();
    assert_eq!(f.reason, "preflight_insufficient_space");
    assert!(e
        .downcast_ref::<ps5upload_ava1::space::SpaceRefusal>()
        .is_some());
}

#[test]
fn a_second_try_keeps_the_first_tries_time_until_the_image_is_done() {
    let data = std::env::temp_dir().join(format!("ps5upload-tocon-stamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let first = console_stamp(Some(&data), "10.0.0.2", "/data/g.exfat");
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        console_stamp(Some(&data), "10.0.0.2:9021", "/data/g.exfat"),
        first
    );
    assert_ne!(
        console_stamp(Some(&data), "10.0.0.2", "/data/h.exfat"),
        first
    );
    forget_console_stamp(Some(&data), "10.0.0.2", "/data/g.exfat");
    assert_ne!(
        console_stamp(Some(&data), "10.0.0.2", "/data/g.exfat"),
        first
    );
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn the_job_id_follows_the_plan_the_console_and_the_path() {
    let a = job_id("192.168.1.5", "/data/x.exfat", "k");
    assert_eq!(a, job_id("192.168.1.5:9021", "/data/x.exfat", "k"));
    assert_ne!(a, job_id("192.168.1.6", "/data/x.exfat", "k"));
    assert_ne!(a, job_id("192.168.1.5", "/data/y.exfat", "k"));
    assert_ne!(a, job_id("192.168.1.5", "/data/x.exfat", "k2"));
}
