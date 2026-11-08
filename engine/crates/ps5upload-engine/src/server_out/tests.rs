//! Convert straight to a server, against the in-memory server every protocol's writer is
//! checked to behave like (the contract tests, and the `--ignored` ones against real servers):
//! every format arrives byte for byte as a local build writes it, a folder file for file, a
//! cancel or a failed read-back leaves nothing, a name already there is never replaced, a
//! write cut off goes on from what the server holds.

use super::*;
use crate::image_build::{self, stream_to_server, ImageFormat, ServerImage, DEFAULT_VERIFY_CHUNK};
use crate::remote::store::{conn, Protocol, Secret};
use crate::remote::MemFs;
use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::FolderSource;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const TIME: i64 = 1_700_000_000;

/// A server with an `/out` folder (made on demand), the destination on it, and a scratch
/// folder here.
struct Nas {
    mem: Arc<MemFs>,
    dest: ServerDest,
    dir: PathBuf,
}

impl Nas {
    fn new(tag: &str) -> Self {
        let mem = Arc::new(MemFs::new(&[("/keep/x", b"x")]));
        let remote = crate::remote::pool::testing::remote_with_shared(Arc::clone(&mem), None);
        let id = remote
            .store
            .add(conn("NAS", Protocol::Nfs), Secret::None)
            .unwrap()
            .conn
            .id;
        let dest =
            ServerDest::with(remote, &id, "/out", tokio::runtime::Handle::current()).unwrap();
        let dir =
            std::env::temp_dir().join(format!("ps5upload-toserver-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { mem, dest, dir }
    }

    /// What the server holds under `/out`.
    fn out(&self) -> Vec<String> {
        self.mem
            .paths()
            .into_iter()
            .filter(|p| p.starts_with("/out/"))
            .collect()
    }
}

impl Drop for Nas {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A small game with `big` bytes that do not repeat, a zero-filled file, an empty file and an
/// empty folder.
fn game(root: &Path, big: usize) -> (PathBuf, Vec<u8>) {
    let game = root.join("PPSA99998-app");
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
        b"{\"titleId\":\"PPSA99998\"}",
    )
    .unwrap();
    std::fs::write(game.join("data/sub/a.bin"), b"").unwrap();
    std::fs::write(game.join("data/zeros.bin"), vec![0u8; 200_000]).unwrap();
    (game, data)
}

fn local_build(src: &Path, format: ImageFormat, out: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(out);
    let mut tree = FolderSource::open(src).unwrap();
    image_build::build_resumable(
        format,
        None,
        Level::default(),
        TIME,
        &mut tree,
        out,
        VerifyMode::Full,
        DEFAULT_VERIFY_CHUNK,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap_or_else(|e| panic!("local {format:?}: {e}"));
    std::fs::read(out).unwrap()
}

fn to_server(
    dest: &ServerDest,
    src: &Path,
    format: ImageFormat,
    name: &str,
    data: Option<PathBuf>,
    cancel: &Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<image_build::ToServer, String> {
    let mut tree = FolderSource::open(src).unwrap();
    let target = ServerImage {
        dest,
        name: name.into(),
        data,
    };
    stream_to_server(
        format,
        TIME,
        "",
        &mut tree,
        &target,
        VerifyMode::Full,
        DEFAULT_VERIFY_CHUNK,
        cancel,
        stage,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn every_format_arrives_as_the_local_build_writes_it() {
    let nas = Nas::new("formats");
    let (src, _) = game(&nas.dir, 3 << 20);
    for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
        let want = local_build(
            &src,
            format,
            &nas.dir.join(format!("local.{}", format.extension())),
        );
        let name = format!("G.{}", format.extension());
        let mut stages: Vec<String> = Vec::new();
        let r = tokio::task::block_in_place(|| {
            to_server(
                &nas.dest,
                &src,
                format,
                &name,
                None,
                &Arc::new(AtomicBool::new(false)),
                &mut |s, _, _| {
                    if stages.last().map(String::as_str) != Some(s) {
                        stages.push(s.to_string());
                    }
                },
            )
        })
        .unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!(stages, ["plan", "write", "verify"], "{format:?}");
        assert_eq!(r.built.image_bytes, want.len() as u64, "{format:?}");
        assert_eq!(r.resumed_from, 0);
        let got = nas.mem.bytes(&format!("/out/{name}")).unwrap();
        assert!(got == want, "{format:?}: the server's image differs");
        assert!(nas.mem.bytes(&format!("/out/{name}.partial")).is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_midway_leaves_no_file() {
    let nas = Nas::new("cancel");
    let (src, _) = game(&nas.dir, 8 << 20);
    let cancel = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&cancel);
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "G.exfat",
            Some(nas.dir.join("data")),
            &cancel,
            &mut |s, done, _| {
                if s == "write" && done > 2 << 20 {
                    c.store(true, Ordering::Relaxed);
                }
            },
        )
    })
    .err()
    .expect("a cancel must fail the build");
    assert_eq!(e, "cancelled");
    assert!(nas.out().is_empty(), "{:?}", nas.out());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_read_back_removes_the_partial_image() {
    let nas = Nas::new("verify");
    let (src, data) = game(&nas.dir, 3 << 20);
    let local = local_build(&src, ImageFormat::Exfat, &nas.dir.join("l.exfat"));
    // A byte of big.bin's data, wherever the image holds it.
    let at = local
        .windows(64)
        .position(|w| w == &data[1_000_000..1_000_064])
        .unwrap() as u64;
    nas.mem.corrupt_writes_at(Some(at + 5));
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "G.exfat",
            Some(nas.dir.join("data")),
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .expect("a corrupted image must fail its read-back");
    assert!(e.contains("differs"), "{e}");
    assert!(e.contains("it was removed from the server NAS"), "{e}");
    assert!(nas.out().is_empty(), "{:?}", nas.out());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_already_there_is_never_replaced() {
    let nas = Nas::new("taken");
    let (src, _) = game(&nas.dir, 1 << 20);
    nas.mem.add(&[("/out/G.exfat", b"mine")]);
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "G.exfat",
            None,
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .unwrap();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(nas.mem.bytes("/out/G.exfat").unwrap(), b"mine");
    // A partial file this engine did not write is left alone too, after the job has stamped the
    // image's time first as a real one does (fpkg_api's to_server_job): that record is no claim on
    // the file.
    nas.mem.add(&[("/out/H.exfat.partial", b"theirs")]);
    let data = nas.dir.join("data");
    image_build::server_stamp(Some(&data), &nas.dest.id, &nas.dest.path_of("H.exfat"));
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "H.exfat",
            Some(nas.dir.join("data")),
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .unwrap();
    assert!(e.contains("did not write it"), "{e}");
    assert_eq!(nas.mem.bytes("/out/H.exfat.partial").unwrap(), b"theirs");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_room_is_refused_before_a_byte_is_written() {
    let nas = Nas::new("room");
    let (src, _) = game(&nas.dir, 1 << 20);
    nas.mem.set_free(Some(1000));
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "G.exfat",
            None,
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .unwrap();
    assert!(e.contains("GiB free"), "{e}");
    assert!(nas.out().is_empty(), "{:?}", nas.out());
}

/// The server goes away part way and does not come back within the job: the job fails, the
/// partial image stays (it could not be removed) with its record, and the next try writes on
/// from it and gives the same bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_cut_off_write_goes_on_from_what_the_server_holds() {
    let nas = Nas::new("resume");
    let (src, _) = game(&nas.dir, 6 << 20);
    let want = local_build(&src, ImageFormat::Ffpfs, &nas.dir.join("l.ffpfs"));
    let data = nas.dir.join("data");
    nas.mem.go_down_after_writes(5);
    let e = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Ffpfs,
            "G.ffpfs",
            Some(data.clone()),
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .expect("a server that stays away fails the build");
    assert!(e.contains("goes on from it"), "{e}");
    nas.mem.set_down(false);
    let held = nas.mem.bytes("/out/G.ffpfs.partial").unwrap().len() as u64;
    assert!(held > 2 << 20, "held {held}");
    let r = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Ffpfs,
            "G.ffpfs",
            Some(data.clone()),
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .unwrap();
    assert!(r.resumed_from > 0, "it started over");
    assert!(r.resumed_from < held);
    assert!(nas.mem.bytes("/out/G.ffpfs").unwrap() == want);
    assert!(nas.mem.bytes("/out/G.ffpfs.partial").is_none());
}

/// A reply lost once (a dropped connection the server comes back from): the build signs in
/// again and writes on, and the image is the one a local build writes.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_is_tried_again_within_the_job() {
    let nas = Nas::new("retry");
    let (src, _) = game(&nas.dir, 6 << 20);
    let want = local_build(&src, ImageFormat::Exfat, &nas.dir.join("l.exfat"));
    nas.mem.go_down_after_writes(4);
    let mem = nas.mem.share();
    // Back once it has gone down, not after a fixed time: on a slow machine a timer can bring
    // it back before the fourth write takes it away, and then nothing brings it back.
    let back = std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !mem.is_down() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        mem.set_down(false);
    });
    let r = tokio::task::block_in_place(|| {
        to_server(
            &nas.dest,
            &src,
            ImageFormat::Exfat,
            "G.exfat",
            Some(nas.dir.join("data")),
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));
    back.join().unwrap();
    assert!(nas.mem.bytes("/out/G.exfat").unwrap() == want);
    assert!(r.built.detail.contains("written to the server NAS"));
}

#[test]
fn the_upload_moves_forward_only_and_keeps_to_its_size() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let nas = rt.block_on(async { Nas::new("upload") });
    let fs = nas.dest.fs().unwrap();
    nas.dest.make_dir(&*fs).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut up =
        Upload::start(&nas.dest, Arc::clone(&fs), "/out/a", 10, 0, cancel.clone()).unwrap();
    up.write_all(b"ab").unwrap();
    assert_eq!(up.seek(SeekFrom::Start(4)).unwrap(), 4);
    assert!(up.seek(SeekFrom::Start(1)).is_err());
    assert_eq!(up.seek(SeekFrom::End(0)).unwrap(), 4);
    assert!(up.write_all(b"0123456789").is_err());
    up.write_all(b"cdefgh").unwrap();
    up.finish().unwrap();
    assert_eq!(nas.mem.bytes("/out/a").unwrap(), b"ab\0\0cdefgh");
    // Short of its size: not finished.
    let mut up =
        Upload::start(&nas.dest, Arc::clone(&fs), "/out/b", 10, 0, cancel.clone()).unwrap();
    up.write_all(b"ab").unwrap();
    assert!(up.finish().is_err());
    // From a byte past the start: the bytes before it only counted.
    let mut up = Upload::start(&nas.dest, Arc::clone(&fs), "/out/a", 10, 4, cancel).unwrap();
    up.write_all(b"XXXXCDEFGH").unwrap();
    up.finish().unwrap();
    assert_eq!(nas.mem.bytes("/out/a").unwrap(), b"ab\0\0CDEFGH");
    drop(nas);
    rt.shutdown_background();
}

// ── Folders ──

fn folder_to_server(
    dest: &ServerDest,
    src: &Path,
    name: &str,
    cancel: &Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    let mut tree = FolderSource::open(src).unwrap();
    let reopen_src = src.to_path_buf();
    let reopen = move || {
        FolderSource::open(&reopen_src)
            .map(|t| Box::new(t) as Box<dyn SourceTree>)
            .map_err(|e| e.to_string())
    };
    let to = ServerFolder {
        dest,
        name: name.into(),
        cancel: Arc::clone(cancel),
    };
    crate::folder_build::build_to(
        &mut tree,
        &reopen,
        &crate::folder_build::FolderOut::Server(&to),
        VerifyMode::Full,
        cancel,
        stage,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_arrives_file_for_file() {
    let nas = Nas::new("folder");
    let (src, _) = game(&nas.dir, 3 << 20);
    let built = tokio::task::block_in_place(|| {
        folder_to_server(
            &nas.dest,
            &src,
            "G",
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built.files, 5);
    for rel in [
        "eboot.bin",
        "sce_sys/param.json",
        "data/sub/big.bin",
        "data/sub/a.bin",
        "data/zeros.bin",
    ] {
        assert_eq!(
            nas.mem.bytes(&format!("/out/G/{rel}")).unwrap(),
            std::fs::read(src.join(rel)).unwrap(),
            "{rel}"
        );
    }
    assert!(nas.out().contains(&"/out/G/empty".to_string()));
    assert!(
        nas.out().iter().all(|p| !p.ends_with(".ps5upload-tmp")),
        "{:?}",
        nas.out()
    );
    // Again: the name is taken now, and nothing of it is touched.
    let e = tokio::task::block_in_place(|| {
        folder_to_server(
            &nas.dest,
            &src,
            "G",
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .unwrap();
    assert!(e.contains("already exists"), "{e}");
    assert!(nas.mem.bytes("/out/G/eboot.bin").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_or_failed_folder_leaves_nothing_and_never_launches() {
    let nas = Nas::new("folder-fail");
    let (src, _) = game(&nas.dir, 3 << 20);
    // The launch files are what is written last: cancelled once big.bin is going, neither is
    // there under its own name at any point.
    let cancel = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&cancel);
    let mem = nas.mem.share();
    let mut launchable = false;
    let e = tokio::task::block_in_place(|| {
        folder_to_server(&nas.dest, &src, "G", &cancel, &mut |s, done, _| {
            launchable |= mem.bytes("/out/G/eboot.bin").is_some()
                || mem.bytes("/out/G/sce_sys/param.json").is_some();
            if s == "write" && done > 1 << 20 {
                c.store(true, Ordering::Relaxed);
            }
        })
    })
    .err()
    .unwrap();
    assert_eq!(e, "cancelled");
    assert!(!launchable);
    assert!(nas.out().is_empty(), "{:?}", nas.out());
    // A server that stores the wrong bytes: the read-back fails and everything goes.
    nas.mem.corrupt_writes_at(Some(3));
    let e = tokio::task::block_in_place(|| {
        folder_to_server(
            &nas.dest,
            &src,
            "G",
            &Arc::new(AtomicBool::new(false)),
            &mut |_, _, _| {},
        )
    })
    .err()
    .unwrap();
    assert!(e.contains("reads back differently"), "{e}");
    assert!(nas.out().is_empty(), "{:?}", nas.out());
}

#[test]
fn launch_files_are_the_ones_a_game_starts_from() {
    assert!(is_launch_file("eboot.bin"));
    assert!(is_launch_file("EBOOT.BIN"));
    assert!(is_launch_file("sce_sys/param.json"));
    assert!(is_launch_file("sce_sys/param.sfo"));
    assert!(!is_launch_file("data/eboot.bin"));
    assert!(!is_launch_file("sce_sys/icon0.png"));
}

// ── A real server ──

/// The file at `path` on the server, read back through the converter's view of it.
fn read_all(dest: &ServerDest, path: &str) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    dest.files()
        .unwrap()
        .open(path)
        .unwrap()
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// Every format and a folder to a real server, byte for byte as built here; a cancel leaves
/// nothing; a name already there is not replaced. Start a server (the Docker recipes in each
/// protocol's `--ignored` contract test), then
/// `PS5UPLOAD_TOSERVER=<smb|nfs|ftp|sftp|webdav>,<host>:<port>,<user>,<password>,<share>,<folder>`
/// with `<folder>` writable (NFS: user `1000:1000`, password empty).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a server; see the doc comment"]
async fn against_a_real_server() {
    let Ok(spec) = std::env::var("PS5UPLOAD_TOSERVER") else {
        return;
    };
    let parts: Vec<&str> = spec.split(',').collect();
    let [proto, addr, user, pass, share, folder] = parts[..] else {
        panic!("PS5UPLOAD_TOSERVER: {spec}");
    };
    let protocol = match proto {
        "smb" => Protocol::Smb,
        "nfs" => Protocol::Nfs,
        "ftp" => Protocol::Ftp,
        "sftp" => Protocol::Sftp,
        "webdav" => Protocol::Webdav,
        p => panic!("unknown protocol {p}"),
    };
    let (host, port) = addr.rsplit_once(':').unwrap();
    let mut c = conn("E2E", protocol.clone());
    c.host = host.into();
    c.port = port.parse().unwrap();
    c.user = user.into();
    c.share = share.into();
    let secret = if pass.is_empty() {
        Secret::None
    } else {
        Secret::Password {
            password: pass.into(),
        }
    };
    if protocol == Protocol::Sftp {
        match crate::remote::sftp_fs::SftpFs::connect(&c, &secret).await {
            Err(RemoteError::HostKey { fingerprint, .. }) => c.host_key = Some(fingerprint),
            Err(e) => panic!("{e}"),
            Ok(_) => {}
        }
    }
    let store = crate::remote::store::Store::open(&crate::remote::store::test_dir()).unwrap();
    let id = store.add(c, secret).unwrap().conn.id;
    let remote = Arc::new(Remote {
        store: Arc::new(store),
        pool: Arc::new(crate::remote::pool::Pool::new(Box::new(
            crate::remote::pool::RealConnector,
        ))),
    });
    let run = format!(
        "{}/ps5u-e2e-{}",
        folder.trim_end_matches('/'),
        std::process::id()
    );
    let dest = ServerDest::with(remote, &id, &run, tokio::runtime::Handle::current()).unwrap();
    let scratch = std::env::temp_dir().join(format!("ps5u-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).unwrap();
    let (src, _) = game(&scratch, 24 << 20);
    let never = Arc::new(AtomicBool::new(false));
    tokio::task::block_in_place(|| {
        let fs = dest.fs().unwrap();
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            let want = local_build(
                &src,
                format,
                &scratch.join(format!("local.{}", format.extension())),
            );
            let name = format!("G.{}", format.extension());
            let started = std::time::Instant::now();
            let r = to_server(&dest, &src, format, &name, None, &never, &mut |_, _, _| {})
                .unwrap_or_else(|e| panic!("{proto} {format:?}: {e}"));
            println!(
                "{proto} {format:?}: {} bytes in {:?} ({})",
                r.built.image_bytes,
                started.elapsed(),
                r.built.detail
            );
            let path = dest.path_of(&name);
            assert!(read_all(&dest, &path) == want, "{proto} {format:?} differs");
            assert!(!dest.exists(&*fs, &format!("{path}.partial")).unwrap());
            // Its name is taken now: refused, and the image stays as it is.
            let e = to_server(&dest, &src, format, &name, None, &never, &mut |_, _, _| {})
                .err()
                .unwrap();
            assert!(e.contains("already exists"), "{e}");
            assert_eq!(
                dest.file_size(&*fs, &path).unwrap(),
                Some(want.len() as u64)
            );
            dest.block_on(fs.remove_file(&path)).unwrap();
        }
        // A cancel part way: nothing left.
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let e = to_server(
            &dest,
            &src,
            ImageFormat::Exfat,
            "C.exfat",
            None,
            &cancel,
            &mut |s, done, _| {
                if s == "write" && done > 8 << 20 {
                    c2.store(true, Ordering::Relaxed);
                }
            },
        )
        .err()
        .unwrap();
        assert_eq!(e, "cancelled");
        assert!(!dest.exists(&*fs, &dest.path_of("C.exfat.partial")).unwrap());
        // A folder, file for file.
        let built = folder_to_server(&dest, &src, "F", &never, &mut |_, _, _| {})
            .unwrap_or_else(|e| panic!("{proto} folder: {e}"));
        assert_eq!(built.files, 5);
        for rel in [
            "eboot.bin",
            "sce_sys/param.json",
            "data/sub/big.bin",
            "data/zeros.bin",
        ] {
            assert!(
                read_all(&dest, &dest.path_of(&format!("F/{rel}")))
                    == std::fs::read(src.join(rel)).unwrap(),
                "{proto} folder {rel}"
            );
        }
        assert!(dest.exists(&*fs, &dest.path_of("F/empty")).unwrap());
        assert!(!dest
            .exists(&*fs, &dest.path_of("F/eboot.bin.ps5upload-tmp"))
            .unwrap());
        // Cleaned up as far as the server lets a client.
        for rel in [
            "F/eboot.bin",
            "F/sce_sys/param.json",
            "F/data/sub/big.bin",
            "F/data/sub/a.bin",
            "F/data/zeros.bin",
        ] {
            let _ = dest.block_on(fs.remove_file(&dest.path_of(rel)));
        }
        for d in ["F/data/sub", "F/data", "F/sce_sys", "F/empty", "F"] {
            let _ = dest.block_on(fs.remove_dir(&dest.path_of(d)));
        }
        let _ = dest.block_on(fs.remove_dir(&dest.dir));
    });
    let _ = std::fs::remove_dir_all(&scratch);
}
