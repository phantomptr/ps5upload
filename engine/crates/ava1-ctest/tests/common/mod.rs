#![allow(dead_code)]
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::keys::Identity;
use ava1::manifest::{self, Manifest};
use ava1::peers::PeerStore;
use ava1::send::{send_job, SendError, SendOptions, SendReport};
use ava1::session::{connect, Timing};
use ava1::source::{LocalSource, Source};
use ava1_ctest::TempDir;

pub const SECRET: [u8; 32] = [0x42; 32];

pub fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(500),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

/// Liveness slow enough for disk-heavy tests on a loaded CI machine.
pub fn calm() -> Timing {
    Timing {
        ping_every: Duration::from_millis(200),
        dead_after: Duration::from_millis(2000),
        handshake: Duration::from_millis(2000),
        ..Timing::default()
    }
}

pub fn dir(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-c-{tag}-{}", std::process::id()))
}

/// A client the C server already knows, and that knows the C server.
pub fn paired_client(peers_file: &Path) -> (Arc<Identity>, Arc<Mutex<PeerStore>>) {
    let me = Arc::new(Identity::generate().unwrap());
    PeerStore::load(peers_file)
        .unwrap()
        .add(me.public(), "rust client")
        .unwrap();
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    (me, Arc::new(Mutex::new(mine)))
}

/// How long one `upload` may take in total before it panics: a missing signal (a dead
/// job, a server that never comes back) fails the test instead of hanging the round.
/// Every real upload here finishes far below this.
const UPLOAD_DEADLINE: Duration = Duration::from_secs(300);

/// Uploads `src` (a directory, or one file) to `root` on the receiver at `addr`,
/// reconnecting after every lost session — the loop Task 22's engine adapter runs.
/// `setup` adjusts the options of each attempt; `job` is the job id for every attempt
/// (a resume must use the same id or it starts a different job).
pub async fn upload(
    addr: &str,
    me: Arc<Identity>,
    peers: Arc<Mutex<PeerStore>>,
    src: &Path,
    root: &str,
    job: [u8; 16],
    setup: impl Fn(&mut SendOptions),
) -> (SendReport, u32) {
    let deadline = tokio::time::Instant::now() + UPLOAD_DEADLINE;
    let mut sessions = 0;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "upload: no finished job in {UPLOAD_DEADLINE:?}"
        );
        let (m, source): (Manifest, Arc<dyn Source>) = if src.is_dir() {
            let s = LocalSource::new(src.to_path_buf());
            (manifest::walk(&s, &|_: &str| false).unwrap(), Arc::new(s))
        } else {
            let s = LocalSource::new(src.parent().unwrap().to_path_buf());
            let name = src.file_name().unwrap().to_str().unwrap().to_string();
            (manifest::single(&s, &name).unwrap(), Arc::new(s))
        };
        // NOTE the 5-arg connect (no trailing None — that is the plan's stale 6-arg form).
        let Ok(s) = connect(addr, me.clone(), peers.clone(), "rust", calm()).await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        sessions += 1;
        let mut link = s.job(job);
        let mut o = SendOptions::upload(root);
        if !src.is_dir() {
            o.flags |= ava1::gen::JF_SINGLE_FILE;
        }
        setup(&mut o);
        match tokio::time::timeout_at(deadline, send_job(&mut link, Arc::new(m), source, o)).await {
            Ok(Ok(r)) => return (r, sessions),
            Ok(Err(SendError::Disconnected(_))) => {
                tokio::time::sleep(Duration::from_millis(200)).await
            }
            Ok(Err(e)) => panic!("upload failed: {e}"),
            Err(_) => panic!("upload: the job did not finish within {UPLOAD_DEADLINE:?}"),
        }
    }
}

pub fn write_tree(dir: &Path, files: usize, size: impl Fn(usize) -> usize) {
    for i in 0..files {
        let p = dir.join(format!("d{:02}/f{i:05}", i % 37));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, (0..size(i)).map(|k| (k ^ i) as u8).collect::<Vec<u8>>()).unwrap();
    }
}

/// True when the two trees hold the same entries with the same bytes (mtimes excluded).
pub fn same_tree(a: &Path, b: &Path) -> bool {
    let (sa, sb) = (
        LocalSource::new(a.to_path_buf()),
        LocalSource::new(b.to_path_buf()),
    );
    let (ma, mb) = (
        manifest::walk(&sa, &|_: &str| false).unwrap(),
        manifest::walk(&sb, &|_: &str| false).unwrap(),
    );
    ma.entries.len() == mb.entries.len()
        && ma.entries.iter().zip(&mb.entries).all(|(x, y)| {
            x.path == y.path
                && x.size == y.size
                && (x.kind == ava1::gen::ENTRY_DIR
                    || std::fs::read(a.join(&x.path)).unwrap()
                        == std::fs::read(b.join(&y.path)).unwrap())
        })
}

/// `cp -r` for a test fixture: `same_tree(src, dst)` before and after.
pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}
