#![cfg(unix)]
//! P3 Task 4: the node, log, net and filesystem management methods on the C payload (host build),
//! driven over AVA1 by a Rust client. `fs.*` are the payload's own native runners
//! (`payload/src/mgmt_fs.c`) acting on a real temp directory; the log/net/node runners run over
//! stub handlers that answer through the real capture sink.
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ava1::gen::{
    self, FsEntry, FsFreeSpace, FsList, FsListResult, FsPath, FsRead, FsReadResult, FsStat,
    FsWrite, MgmtText,
};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session, Timing};
use ava1::wire::Message;
use ava1_ctest::*;

const SECRET: [u8; 32] = [0x43; 32];
const OK: u16 = gen::STATUS_OK;

/// The installed table, policy and stub counters are process-wide (test_shim_fs.c): every test holds
/// the shared shim guard for its whole run, so the suite passes in parallel mode.
fn one() -> MutexGuard<'static, ()> {
    CServer::lock_for_shim_tests()
}

struct Rig {
    _one: MutexGuard<'static, ()>,
    _srv: CServer,
    s: Session,
    root: PathBuf,
}

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(3000),
        handshake: Duration::from_millis(800),
        ..Timing::default()
    }
}

// The process-wide lock is held for the whole test on purpose (the installed table is global).
#[allow(clippy::await_holding_lock)]
async fn rig(tag: &str) -> Rig {
    let one = one();
    let base = std::env::temp_dir().join(format!("ava1-mfs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    // The policy compares against the path as the C code sees it: the canonical one.
    let root = root.canonicalize().unwrap();
    assert_eq!(mgmt_fs::install(&root), 0);
    mgmt_fs::set(false, 0, 0);
    let me = Arc::new(Identity::generate().unwrap());
    PeerStore::load(&base.join("peers"))
        .unwrap()
        .add(me.public(), "rust client")
        .unwrap();
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    let srv = CServer::start(SECRET, &base.join("peers"), 0, 100, 3000, 800);
    let s = connect(
        &srv.addr(),
        me,
        Arc::new(Mutex::new(mine)),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    Rig {
        _one: one,
        _srv: srv,
        s,
        root,
    }
}

impl Rig {
    fn p(&self, rel: &str) -> String {
        format!("{}/{rel}", self.root.display())
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
    async fn rpc<M: Message>(&self, method: u16, m: &M) -> (u16, Vec<u8>) {
        let r = self.s.rpc(method, &m.to_bytes().unwrap()).await.unwrap();
        (r.status, r.body)
    }
    async fn raw(&self, method: u16, body: &[u8]) -> (u16, Vec<u8>) {
        let r = self.s.rpc(method, body).await.unwrap();
        (r.status, r.body)
    }
    async fn list(&self, path: &str, offset: u32, limit: u16) -> FsListResult {
        let (st, b) = self
            .rpc(
                gen::METHOD_FS_LIST,
                &FsList {
                    path: path.into(),
                    offset,
                    limit,
                },
            )
            .await;
        assert_eq!(st, OK, "{}", String::from_utf8_lossy(&b));
        FsListResult::decode(&b).unwrap()
    }
    async fn write(
        &self,
        rel: &str,
        offset: u64,
        flags: u32,
        data: &[u8],
        mode: Option<u32>,
    ) -> (u16, Vec<u8>) {
        self.rpc(
            gen::METHOD_FS_WRITE,
            &FsWrite {
                path: self.p(rel),
                offset,
                flags,
                data: data.to_vec(),
                mode,
            },
        )
        .await
    }
    async fn read(&self, rel: &str, offset: u64, len: u32, flags: u32) -> (u16, Vec<u8>) {
        self.rpc(
            gen::METHOD_FS_READ,
            &FsRead {
                path: self.p(rel),
                offset,
                len,
                flags,
            },
        )
        .await
    }
}

fn cause(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn text(s: &str) -> Vec<u8> {
    MgmtText {
        body: s.as_bytes().to_vec(),
        more: None,
    }
    .to_bytes()
    .unwrap()
}

// ---- fs.list ----

#[tokio::test(flavor = "multi_thread")]
async fn list_dir_pages_cover_a_20k_entry_directory() {
    let r = rig("list20k").await;
    let d = r.path("big");
    std::fs::create_dir_all(&d).unwrap();
    for i in 0..20_000u32 {
        std::fs::write(d.join(format!("f{i:05}")), b"").unwrap();
    }
    let mut seen = std::collections::BTreeSet::new();
    let (mut offset, mut pages) = (0u32, 0);
    loop {
        let page = r.list(&r.p("big"), offset, 256).await;
        assert!(page.entries.len() <= 256);
        pages += 1;
        for e in &page.entries {
            assert!(seen.insert(e.name.clone()), "{} listed twice", e.name);
        }
        offset += page.entries.len() as u32;
        if page.more == 0 {
            assert_eq!(
                page.total_scanned, 20_000,
                "the last page counts everything"
            );
            break;
        }
        assert_eq!(page.entries.len(), 256, "a page that says more is full");
    }
    assert_eq!(seen.len(), 20_000);
    assert_eq!(pages, 79, "20,000 / 256 rounded up");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_reports_kinds_sizes_mtime_and_mode_and_defaults_the_limit() {
    let r = rig("listkinds").await;
    std::fs::create_dir_all(r.path("d/sub")).unwrap();
    std::fs::write(r.path("d/file"), b"12345").unwrap();
    std::fs::set_permissions(r.path("d/file"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("file", r.path("d/link")).unwrap();
    std::os::unix::fs::symlink("/nonexistent-target", r.path("d/dangling")).unwrap();
    let page = r.list(&r.p("d"), 0, 0).await; // limit 0 = the default (256)
    let by: std::collections::HashMap<String, &FsEntry> =
        page.entries.iter().map(|e| (e.name.clone(), e)).collect();
    assert_eq!(by.len(), 4);
    assert_eq!((by["file"].kind, by["file"].size), (gen::ENTRY_FILE, 5));
    assert_eq!(by["file"].mode, Some(0o640));
    assert!(by["file"].mtime.unwrap() > 1_600_000_000);
    assert_eq!(by["sub"].kind, gen::ENTRY_DIR);
    // a symbolic link is reported as a link, not followed (a dangling one too)
    assert_eq!(by["link"].kind, gen::ENTRY_LINK);
    assert_eq!(by["dangling"].kind, gen::ENTRY_LINK);
    assert_eq!(page.more, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_refuses_what_the_ftx2_handler_refused() {
    let r = rig("listerr").await;
    let ask = |p: &str| FsList {
        path: p.into(),
        offset: 0,
        limit: 10,
    };
    let (st, b) = r.rpc(gen::METHOD_FS_LIST, &ask("relative/path")).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_list_dir_bad_path")
    );
    let (st, b) = r.rpc(gen::METHOD_FS_LIST, &ask("/a/../b")).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_list_dir_path_denied")
    );
    let (st, b) = r.rpc(gen::METHOD_FS_LIST, &ask(&r.p("missing"))).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_list_dir_opendir_errno_2")
    );
    // a name that merely contains ".." is fine
    std::fs::create_dir_all(r.path("..cache/x..bak")).unwrap();
    assert_eq!(r.list(&r.p("..cache"), 0, 10).await.entries.len(), 1);
    let (st, _) = r.raw(gen::METHOD_FS_LIST, &[1, 2]).await;
    assert_eq!(st, gen::ERR_PROTOCOL);
}

// ---- fs.freespace ----

#[tokio::test(flavor = "multi_thread")]
async fn fs_freespace_is_the_post_reserve_figure_of_the_drive_holding_the_path() {
    let r = rig("freespace").await;
    std::fs::create_dir_all(r.path("d")).unwrap();
    let ask = |rel: &str| FsPath { path: r.p(rel) };
    let (st, b) = r.rpc(gen::METHOD_FS_FREESPACE, &ask("d")).await;
    assert_eq!(st, OK);
    let f = FsFreeSpace::decode(&b).unwrap();
    assert!(f.total > 0 && f.free > 0, "{f:?}");
    // The reserve rule: 1/64th of the drive, at most 1 GiB; usable is free less that, never raw free.
    assert_eq!(f.reserve, (f.total / 64).min(1 << 30));
    assert_eq!(f.usable, f.free.saturating_sub(f.reserve));
    assert!(f.usable < f.free);
    assert_eq!(f.dev, std::fs::metadata(r.path("d")).unwrap().dev());
    // A destination that does not exist yet is answered for its nearest existing ancestor.
    let (st, b) = r
        .rpc(gen::METHOD_FS_FREESPACE, &ask("d/new/deeper/file"))
        .await;
    assert_eq!(st, OK);
    assert_eq!(FsFreeSpace::decode(&b).unwrap().dev, f.dev);
    // Relative and climbing paths are refused like fs.stat's.
    let (st, _) = r
        .rpc(gen::METHOD_FS_FREESPACE, &FsPath { path: "rel".into() })
        .await;
    assert_eq!(st, gen::ERR_PATH);
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_FREESPACE,
            &FsPath {
                path: "/a/../b".into(),
            },
        )
        .await;
    assert_eq!(st, gen::ERR_PATH);
    let (st, _) = r.raw(gen::METHOD_FS_FREESPACE, &[1, 2]).await;
    assert_eq!(st, gen::ERR_PROTOCOL);
}

// ---- fs.stat ----

#[tokio::test(flavor = "multi_thread")]
async fn fs_stat_reports_dev() {
    let r = rig("stat").await;
    std::fs::write(r.path("f"), b"hello").unwrap();
    std::fs::create_dir_all(r.path("d")).unwrap();
    std::os::unix::fs::symlink("/nonexistent-target", r.path("dangling")).unwrap();
    let stat = |rel: &str| FsPath { path: r.p(rel) };
    let (st, b) = r.rpc(gen::METHOD_FS_STAT, &stat("f")).await;
    assert_eq!(st, OK);
    let s = FsStat::decode(&b).unwrap();
    let md = std::fs::metadata(r.path("f")).unwrap();
    assert_eq!((s.kind, s.size, s.dev), (gen::ENTRY_FILE, 5, md.dev()));
    assert_eq!(s.mode, md.mode() & 0o7777);
    assert_eq!(s.mtime, md.mtime() as u64);
    let (_, b) = r.rpc(gen::METHOD_FS_STAT, &stat("d")).await;
    assert_eq!(FsStat::decode(&b).unwrap().kind, gen::ENTRY_DIR);
    let (st, b) = r.rpc(gen::METHOD_FS_STAT, &stat("dangling")).await;
    assert_eq!(st, OK, "a dangling link exists as a link");
    assert_eq!(FsStat::decode(&b).unwrap().kind, gen::ENTRY_LINK);
    // a missing path is an error, not an empty OK: that is what the 1-byte FsRead probe was
    let (st, b) = r.rpc(gen::METHOD_FS_STAT, &stat("nope")).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_stat_failed_errno_2")
    );
    let (st, _) = r
        .rpc(gen::METHOD_FS_STAT, &FsPath { path: "rel".into() })
        .await;
    assert_eq!(st, gen::ERR_PATH);
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_STAT,
            &FsPath {
                path: "/a/../b".into(),
            },
        )
        .await;
    assert_eq!(st, gen::ERR_PATH);
}

// ---- fs.mkdir ----

#[tokio::test(flavor = "multi_thread")]
async fn mkdir_honours_mode_and_parents() {
    let r = rig("mkdir").await;
    let mk = |rel: &str, mode: u32, parents: u8| gen::FsMkdir {
        path: r.p(rel),
        mode,
        parents,
    };
    let (st, b) = r.rpc(gen::METHOD_FS_MKDIR, &mk("a/b/c", 0o750, 1)).await;
    assert_eq!((st, b.len()), (OK, 0));
    assert_eq!(
        std::fs::metadata(r.path("a/b/c")).unwrap().mode() & 0o7777,
        0o750,
        "mode applies to the new directory (the umask does not eat it)"
    );
    // parents = 0: a missing parent is an error and nothing is created
    let (st, b) = r.rpc(gen::METHOD_FS_MKDIR, &mk("x/y", 0o755, 0)).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_IO, "fs_mkdir_failed"));
    assert!(!r.path("x").exists());
    // an existing directory is fine (mkdir -p) and keeps its mode; a file in the way is not
    let (st, _) = r.rpc(gen::METHOD_FS_MKDIR, &mk("a/b/c", 0o700, 1)).await;
    assert_eq!(st, OK);
    assert_eq!(
        std::fs::metadata(r.path("a/b/c")).unwrap().mode() & 0o7777,
        0o750
    );
    std::fs::write(r.path("file"), b"").unwrap();
    let (st, b) = r.rpc(gen::METHOD_FS_MKDIR, &mk("file", 0o755, 1)).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_EXISTS, "fs_mkdir_exists_not_dir")
    );
    // outside the policy
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_MKDIR,
            &gen::FsMkdir {
                path: "/etc/nope".into(),
                mode: 0o755,
                parents: 1,
            },
        )
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_mkdir_path_not_allowed")
    );
}

// ---- fs.rename ----

fn rename(r: &Rig, from: &str, to: &str, overwrite: u8) -> gen::FsRename {
    gen::FsRename {
        from: r.p(from),
        to: r.p(to),
        overwrite,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fs_rename_cross_device_is_refused_before_any_rename() {
    let r = rig("xdev").await;
    std::fs::create_dir_all(r.path("mnt2")).unwrap();
    std::fs::write(r.path("src"), b"data").unwrap();
    // The guard compares the source's device with the destination's parent: here "/mnt2" is another device.
    mgmt_fs::set(true, 0, 0);
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", "mnt2/dst", 1))
        .await;
    assert_eq!(st, gen::ERR_CROSS_DEVICE);
    assert_eq!(cause(&b), "fs_move_cross_mount");
    assert!(r.path("src").exists(), "nothing moved");
    assert!(!r.path("mnt2/dst").exists());
    // Same device: it renames.
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", "dst", 1))
        .await;
    assert_eq!((st, b.len()), (OK, 0));
    assert!(r.path("dst").exists() && !r.path("src").exists());
    // And an unknown device (a missing source) is not read as "same": it is refused (review 007 #4).
    mgmt_fs::set(false, 0, 0); // the real lstat/stat: a missing source cannot be read
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "ghost", "dst2", 1))
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_move_device_unknown")
    );
}

/// DISCRIMINATING (review 007 #4, final review fs #1): a destination whose parent path is longer than
/// the guard's old 512-byte buffer was clamped, so a parent on another mount ("/mnt2" here) was cut off,
/// read as the same device, and the rename went through. The host rename would succeed, so a fail-open
/// guard is visible as the file having moved.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_destination_path_is_judged_whole_not_truncated() {
    let r = rig("xdev-long").await;
    let parent = format!(
        "{}/{}/{}/mnt2",
        "a".repeat(200),
        "b".repeat(200),
        "c".repeat(200)
    );
    std::fs::create_dir_all(r.path(&parent)).unwrap();
    std::fs::write(r.path("src"), b"data").unwrap();
    mgmt_fs::set(true, 0, 0); // a path containing "/mnt2" is another device
    let to = format!("{parent}/dst");
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", &to, 1))
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_CROSS_DEVICE, "fs_move_cross_mount")
    );
    assert!(r.path("src").exists() && !r.path(&to).exists(), "moved");
}

/// DISCRIMINATING (review 007 #4): the devices cannot be read, yet the source exists and a host
/// rename would succeed. Fail open would move the file; fail closed leaves it.
#[tokio::test(flavor = "multi_thread")]
async fn fs_rename_with_an_unreadable_device_is_refused_and_moves_nothing() {
    let r = rig("xdev-unknown").await;
    std::fs::write(r.path("src"), b"data").unwrap();
    mgmt_fs::set_unreadable_devices();
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", "dst", 1))
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_move_device_unknown")
    );
    assert!(r.path("src").exists() && !r.path("dst").exists(), "moved");
}

#[tokio::test(flavor = "multi_thread")]
async fn fs_rename_overwrite_and_policy() {
    let r = rig("rename").await;
    std::fs::write(r.path("a"), b"A").unwrap();
    std::fs::write(r.path("b"), b"B").unwrap();
    let (st, b) = r.rpc(gen::METHOD_FS_RENAME, &rename(&r, "a", "b", 0)).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_EXISTS, "fs_move_exists")
    );
    assert_eq!(std::fs::read(r.path("b")).unwrap(), b"B");
    let (st, _) = r.rpc(gen::METHOD_FS_RENAME, &rename(&r, "a", "b", 1)).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(r.path("b")).unwrap(), b"A");
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_RENAME,
            &gen::FsRename {
                from: r.p("b"),
                to: "/etc/passwd2".into(),
                overwrite: 1,
            },
        )
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_move_path_not_allowed")
    );
}

// ---- fs.chmod ----

#[tokio::test(flavor = "multi_thread")]
async fn fs_chmod_sets_the_bits_and_obeys_the_policy() {
    let r = rig("chmod").await;
    std::fs::write(r.path("f"), b"").unwrap();
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_CHMOD,
            &gen::FsChmod {
                path: r.p("f"),
                mode: 0o600,
            },
        )
        .await;
    assert_eq!((st, b.len()), (OK, 0));
    assert_eq!(
        std::fs::metadata(r.path("f")).unwrap().mode() & 0o7777,
        0o600
    );
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_CHMOD,
            &gen::FsChmod {
                path: r.p("missing"),
                mode: 0o600,
            },
        )
        .await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_IO, "fs_chmod_failed"));
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_CHMOD,
            &gen::FsChmod {
                path: "/etc/hosts".into(),
                mode: 0o777,
            },
        )
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_chmod_path_not_allowed")
    );
}

// ---- fs.read ----

#[tokio::test(flavor = "multi_thread")]
async fn fs_read_windows_eof_and_the_unsafe_flag() {
    let r = rig("read").await;
    let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(r.path("big"), &data).unwrap();
    // a window in the middle: not the end
    let (st, b) = r.read("big", 10, 100, 0).await;
    assert_eq!(st, OK);
    let m = FsReadResult::decode(&b).unwrap();
    assert_eq!((&m.data[..], m.eof), (&data[10..110], 0));
    // the cap: a longer ask is a short read (eof 0), never an error
    let (_, b) = r.read("big", 0, u32::MAX, 0).await;
    let m = FsReadResult::decode(&b).unwrap();
    assert_eq!((m.data.len(), m.eof), (gen::FS_READ_MAX as usize, 0));
    assert_eq!(m.data, &data[..gen::FS_READ_MAX as usize]);
    // the last bytes carry eof; an ask ending exactly at the end does too
    let (_, b) = r.read("big", 999_900, 1000, 0).await;
    let m = FsReadResult::decode(&b).unwrap();
    assert_eq!((m.data.len(), m.eof), (100, 1));
    let (_, b) = r.read("big", 999_900, 100, 0).await;
    assert_eq!(FsReadResult::decode(&b).unwrap().eof, 1);
    // at and past the end: empty with eof
    for off in [1_000_000u64, 5_000_000] {
        let (st, b) = r.read("big", off, 10, 0).await;
        let m = FsReadResult::decode(&b).unwrap();
        assert_eq!((st, m.data.len(), m.eof), (OK, 0, 1));
    }
    // errors keep the legacy tokens
    let (st, b) = r.read("nope", 0, 10, 0).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_read_stat_failed")
    );
    std::fs::create_dir_all(r.path("dir")).unwrap();
    let (st, b) = r.read("dir", 0, 10, 0).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_read_not_regular_file")
    );
    // the system tree is readable only with FSR_UNSAFE (the policy sees the flag)
    std::fs::create_dir_all(r.path("sys")).unwrap();
    std::fs::write(r.path("sys/lib"), b"elf").unwrap();
    let (st, b) = r.read("sys/lib", 0, 10, 0).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_read_path_not_allowed")
    );
    let (st, b) = r.read("sys/lib", 0, 10, gen::FSR_UNSAFE).await;
    assert_eq!(st, OK);
    assert_eq!(FsReadResult::decode(&b).unwrap().data, b"elf");
    assert!(
        mgmt_fs::stats().2 >= 1,
        "the unsafe flag reached the policy"
    );
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_READ,
            &FsRead {
                path: "/etc/hosts".into(),
                offset: 0,
                len: 10,
                flags: 0,
            },
        )
        .await;
    assert_eq!(st, gen::ERR_PATH);
}

// ---- fs.write ----

const CREATE: u32 = gen::FSW_CREATE;
const OVERWRITE: u32 = gen::FSW_OVERWRITE;
const AT: u32 = gen::FSW_AT_OFFSET;
const COMMIT: u32 = gen::FSW_COMMIT;

#[tokio::test(flavor = "multi_thread")]
async fn fs_write_whole_file_is_atomic_and_honours_create_and_mode() {
    let r = rig("write1").await;
    let (st, b) = r.write("w", 0, OVERWRITE, b"hello", None).await;
    assert_eq!((st, b.len()), (OK, 0));
    assert_eq!(std::fs::read(r.path("w")).unwrap(), b"hello");
    assert_eq!(
        std::fs::metadata(r.path("w")).unwrap().mode() & 0o7777,
        0o644
    );
    assert!(
        !r.path("w.ps5upload.tmp").exists(),
        "committed in the same call"
    );
    // default (neither flag) overwrites; CREATE refuses an existing file and writes nothing
    let (st, _) = r.write("w", 0, 0, b"v2", Some(0o600)).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(r.path("w")).unwrap(), b"v2");
    assert_eq!(
        std::fs::metadata(r.path("w")).unwrap().mode() & 0o7777,
        0o600
    );
    let (st, b) = r.write("w", 0, CREATE, b"v3", None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_EXISTS, "exists"));
    assert_eq!(std::fs::read(r.path("w")).unwrap(), b"v2");
    assert!(!r.path("w.ps5upload.tmp").exists());
    let (st, _) = r.write("new", 0, CREATE, b"x", None).await;
    assert_eq!(st, OK);
    // an empty file is a valid file
    let (st, _) = r.write("empty", 0, 0, b"", None).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(r.path("empty")).unwrap(), b"");
    assert!(
        mgmt_fs::stats().0 >= 4,
        "successful writes are counted as commands"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fs_write_chunks_commit_on_the_last_and_a_retry_starts_clean() {
    let r = rig("write2").await;
    let part = |i: usize| vec![b'a' + i as u8; 1000];
    // chunk 0 at offset 0, chunk 1 at 1000, last (with COMMIT) at 2000
    let (st, _) = r.write("c", 0, OVERWRITE | AT, &part(0), None).await;
    assert_eq!(st, OK);
    assert!(!r.path("c").exists(), "not committed yet");
    assert_eq!(
        std::fs::metadata(r.path("c.ps5upload.tmp")).unwrap().len(),
        1000
    );
    let (st, _) = r.write("c", 1000, OVERWRITE | AT, &part(1), None).await;
    assert_eq!(st, OK);
    // an abandoned attempt: a new chunk at offset 0 truncates the tmp file first
    let (st, _) = r.write("c", 0, OVERWRITE | AT, &part(0), None).await;
    assert_eq!(st, OK);
    assert_eq!(
        std::fs::metadata(r.path("c.ps5upload.tmp")).unwrap().len(),
        1000
    );
    let (st, _) = r.write("c", 1000, OVERWRITE | AT, &part(1), None).await;
    assert_eq!(st, OK);
    let (st, _) = r
        .write("c", 2000, OVERWRITE | AT | COMMIT, &part(2), Some(0o640))
        .await;
    assert_eq!(st, OK);
    let mut want = part(0);
    want.extend(part(1));
    want.extend(part(2));
    assert_eq!(std::fs::read(r.path("c")).unwrap(), want);
    assert!(!r.path("c.ps5upload.tmp").exists());
    assert_eq!(
        std::fs::metadata(r.path("c")).unwrap().mode() & 0o7777,
        0o640
    );
    // CREATE is checked at commit: the target appeared while chunks were sent
    let (st, _) = r.write("d", 0, CREATE | AT, b"zz", None).await;
    assert_eq!(st, OK);
    std::fs::write(r.path("d"), b"someone else").unwrap();
    let (st, b) = r.write("d", 2, CREATE | AT | COMMIT, b"yy", None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_EXISTS, "exists"));
    assert_eq!(std::fs::read(r.path("d")).unwrap(), b"someone else");
    assert!(
        !r.path("d.ps5upload.tmp").exists(),
        "a refused commit leaves no tmp"
    );
    // APPEND adds to the end whatever offset says
    let (st, _) = r
        .write("e", 0, OVERWRITE | gen::FSW_APPEND, b"12", None)
        .await;
    assert_eq!(st, OK);
    let (st, _) = r
        .write("e", 0, OVERWRITE | gen::FSW_APPEND | COMMIT, b"34", None)
        .await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(r.path("e")).unwrap(), b"1234");
}

#[tokio::test(flavor = "multi_thread")]
async fn fs_write_over_48k_is_refused_not_truncated() {
    let r = rig("write3").await;
    let big = vec![7u8; gen::FSW_CHUNK_MAX as usize + 1];
    let (st, b) = r.write("big", 0, 0, &big, None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_PROTOCOL, "too_large"));
    assert!(!r.path("big").exists() && !r.path("big.ps5upload.tmp").exists());
    // exactly one chunk is fine
    let ok = vec![7u8; gen::FSW_CHUNK_MAX as usize];
    let (st, _) = r.write("big", 0, 0, &ok, None).await;
    assert_eq!(st, OK);
    assert_eq!(
        std::fs::metadata(r.path("big")).unwrap().len(),
        ok.len() as u64
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fs_write_validates_flags_and_paths() {
    let r = rig("write4").await;
    let (st, b) = r.write("f", 0, CREATE | OVERWRITE, b"x", None).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PROTOCOL, "fs_write_flags_conflict")
    );
    let (st, _) = r.write("f", 0, AT | gen::FSW_APPEND, b"x", None).await;
    assert_eq!(st, gen::ERR_PROTOCOL);
    let (st, b) = r.write("f", 5, 0, b"x", None).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PROTOCOL, "fs_write_offset_without_chunk")
    );
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_WRITE,
            &FsWrite {
                path: "/etc/evil".into(),
                offset: 0,
                flags: 0,
                data: vec![1],
                mode: None,
            },
        )
        .await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_PATH, "path_unsafe"));
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_WRITE,
            &FsWrite {
                path: String::new(),
                offset: 0,
                flags: 0,
                data: vec![1],
                mode: None,
            },
        )
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PROTOCOL, "path_required")
    );
    // a directory in the way: the rename fails and leaves no tmp
    std::fs::create_dir_all(r.path("dir/inner")).unwrap();
    let (st, b) = r.write("dir", 0, 0, b"x", None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_IO, "rename_failed"));
    assert!(!r.path("dir.ps5upload.tmp").exists());
}

// ---- node, log, net ----

#[tokio::test(flavor = "multi_thread")]
async fn failures_that_carry_data_keep_it_in_the_cause() {
    let r = rig("keep").await;
    // net.reach is a probe (Task 9's mgmt_call_probe): an unreachable host is an ANSWER carrying timed_out/errno/ms
    let (st, b) = r
        .raw(gen::METHOD_NET_REACH, &text(r#"{"host":"unreachable"}"#))
        .await;
    assert_eq!(st, OK);
    let v: serde_json::Value = serde_json::from_slice(&MgmtText::decode(&b).unwrap().body).unwrap();
    assert_eq!(
        (
            v["ok"].as_bool(),
            v["timed_out"].as_bool(),
            v["ms"].as_u64()
        ),
        (Some(false), Some(true), Some(3000))
    );
    let (st, b) = r
        .raw(gen::METHOD_NET_REACH, &text(r#"{"host":"10.0.0.1"}"#))
        .await;
    assert_eq!(st, OK);
    assert_eq!(MgmtText::decode(&b).unwrap().body, br#"{"ok":true,"ms":4}"#);
    // a malformed request is an error status with its token
    let (st, b) = r.raw(gen::METHOD_NET_REACH, &text("{}")).await;
    assert_eq!(st, gen::ERR_PROTOCOL);
    assert_eq!(cause(&b), "bad_request");
    // a mount's code and mount point
    let (st, b) = r.raw(gen::METHOD_FS_MOUNT_PKG, &text("{}")).await;
    assert_ne!(st, OK);
    let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["code"].as_i64(), Some(-2147352567));
    assert_eq!(v["mount_point"], "/mnt/ps5upload/x.pkg.mount");
}

#[tokio::test(flavor = "multi_thread")]
async fn node_and_net_methods_answer_and_map_their_errors() {
    let r = rig("node").await;
    // node.shutdown: an empty reply, the handler ran once
    let (st, b) = r.raw(gen::METHOD_NODE_SHUTDOWN, &[]).await;
    assert_eq!((st, b.len()), (OK, 0));
    assert_eq!(mgmt_fs::stats().1, 1);
    // node.cleanup: text in and out, tokens mapped
    let (st, b) = r
        .raw(gen::METHOD_NODE_CLEANUP, &text(r#"{"path":"/data/x"}"#))
        .await;
    assert_eq!(st, OK);
    assert!(cause(&MgmtText::decode(&b).unwrap().body).contains("removed_files"));
    let (st, b) = r
        .raw(gen::METHOD_NODE_CLEANUP, &text(r#"{"path":"/denied"}"#))
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "cleanup_path_denied")
    );
    let (st, b) = r.raw(gen::METHOD_NODE_CLEANUP, &text("{}")).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PROTOCOL, "cleanup_missing_path")
    );
    // net.interfaces and net.speedtest are text methods
    let (st, b) = r.raw(gen::METHOD_NET_INTERFACES, &text("")).await;
    assert_eq!(st, OK);
    assert!(cause(&MgmtText::decode(&b).unwrap().body).contains("em0"));
    let (st, b) = r.raw(gen::METHOD_NET_SPEEDTEST, &[]).await;
    assert_eq!(st, OK);
    assert_eq!(MgmtText::decode(&b).unwrap().body, br#"{"ok":true}"#);
}

#[allow(dead_code)]
fn _unused(_: &Path) {}

// ---- the Rust transport against the C payload (core call sites, end to end) ----

mod transport {
    use super::*;
    use ps5upload_ava1::mgmt::AvaTransport;
    use ps5upload_ava1::Pool;
    use ps5upload_core::mgmt::{self, MgmtError};

    pub struct T {
        _one: MutexGuard<'static, ()>,
        _srv: CServer,
        _scope: mgmt::ScopedTransport,
        pub transport: Arc<AvaTransport>,
        pub console: String,
        pub root: PathBuf,
    }

    /// `install`: the server's management table is installed (CAP_MGMT is advertised).
    pub fn rig(tag: &str, install: bool) -> T {
        let one = one();
        let base = std::env::temp_dir().join(format!("ava1-mfst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        if install {
            assert_eq!(mgmt_fs::install(&root), 0);
        } else {
            mgmt_fs::uninstall();
        }
        mgmt_fs::set(false, 0, 0);
        let ava = base.join("ava");
        std::fs::create_dir_all(&ava).unwrap();
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        PeerStore::load(&base.join("srv-peers"))
            .unwrap()
            .add(me.public(), "engine")
            .unwrap();
        PeerStore::load(&ava.join("peers"))
            .unwrap()
            .add(Identity::from_secret(SECRET).public(), "C test server")
            .unwrap();
        let srv = CServer::start(SECRET, &base.join("srv-peers"), 0, 100, 3000, 800);
        let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(srv.addr())));
        let transport = Arc::new(AvaTransport::with_pool(pool));
        let scope = mgmt::scoped_transport(transport.clone());
        T {
            _one: one,
            _srv: srv,
            _scope: scope,
            transport,
            console: "c-console:9114".into(),
            root,
        }
    }

    impl T {
        pub fn p(&self, rel: &str) -> String {
            format!("{}/{rel}", self.root.display())
        }
    }

    #[test]
    fn list_dir_through_core_covers_a_directory_in_pages() {
        let t = rig("t-list", true);
        let d = t.root.join("many");
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..700u32 {
            std::fs::write(d.join(format!("e{i:04}")), b"x").unwrap();
        }
        let mut got = 0usize;
        let mut offset = 0u64;
        loop {
            let l = ps5upload_core::fs_ops::list_dir(
                &t.console,
                &t.p("many"),
                ps5upload_core::fs_ops::ListDirOptions { offset, limit: 256 },
            )
            .unwrap();
            got += l.entries.len();
            offset += l.entries.len() as u64;
            if !l.truncated {
                break;
            }
        }
        assert_eq!(got, 700);
    }

    #[test]
    fn fs_read_loops_to_2_mib() {
        let t = rig("t-read", true);
        let data: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(t.root.join("big"), &data).unwrap();
        // the FTX2 per-call ceiling is held: 3 MiB file, 5 MiB ask, 2 MiB back (8 round trips of <= 256 KiB)
        let got =
            ps5upload_core::fs_ops::fs_read(&t.console, &t.p("big"), 0, 5 * 1024 * 1024).unwrap();
        assert_eq!(got.len(), 2 * 1024 * 1024);
        assert_eq!(got, &data[..2 * 1024 * 1024]);
        // a window near the end stops at eof
        let tail =
            ps5upload_core::fs_ops::fs_read(&t.console, &t.p("big"), 3 * 1024 * 1024 - 10, 100)
                .unwrap();
        assert_eq!(tail, &data[data.len() - 10..]);
        // a refusal keeps the text callers match on
        let e = ps5upload_core::fs_ops::fs_read(&t.console, "/etc/hosts", 0, 10).unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected FS_READ(/etc/hosts): fs_read_path_not_allowed"
        );
    }

    #[test]
    fn fs_write_bytes_chunks_above_48_kib_and_reports_create_refusals() {
        let t = rig("t-write", true);
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 241) as u8).collect();
        let r = ps5upload_core::diagnostics::fs_write_bytes(
            &t.console,
            &t.p("cfg/out.bin"),
            &data,
            false,
        );
        // the parent directory does not exist yet: a plain refusal, nothing half-written
        assert!(r.is_err() || !t.root.join("cfg/out.bin.ps5upload.tmp").exists());
        ps5upload_core::fs_ops::fs_mkdir(&t.console, &t.p("cfg")).unwrap();
        ps5upload_core::diagnostics::fs_write_bytes(&t.console, &t.p("cfg/out.bin"), &data, false)
            .unwrap();
        assert_eq!(std::fs::read(t.root.join("cfg/out.bin")).unwrap(), data);
        assert!(!t.root.join("cfg/out.bin.ps5upload.tmp").exists());
        // create-only on an existing file: the legacy `exists` failure
        let e = ps5upload_core::diagnostics::fs_write_bytes(
            &t.console,
            &t.p("cfg/out.bin"),
            b"x",
            true,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("exists"), "{e:#}");
        assert_eq!(
            std::fs::read(t.root.join("cfg/out.bin")).unwrap(),
            data,
            "untouched"
        );
    }

    #[test]
    fn fs_write_bytes_large_copies_a_700_kb_file_that_the_plain_call_refuses() {
        let t = rig("t-write-large", true);
        let data: Vec<u8> = (0..700_000u32).map(|i| (i % 239) as u8).collect();
        let e =
            ps5upload_core::diagnostics::fs_write_bytes(&t.console, &t.p("big.bin"), &data, false)
                .unwrap_err();
        assert!(format!("{e:#}").contains("too_large"), "{e:#}");
        ps5upload_core::diagnostics::fs_write_bytes_large(
            &t.console,
            &t.p("big.bin"),
            &data,
            false,
        )
        .unwrap();
        assert_eq!(std::fs::read(t.root.join("big.bin")).unwrap(), data);
        assert!(!t.root.join("big.bin.ps5upload.tmp").exists());
    }

    #[test]
    fn fs_stat_exists_and_the_not_found_text() {
        let t = rig("t-stat", true);
        std::fs::write(t.root.join("f"), b"abc").unwrap();
        let s = ps5upload_core::fs_ops::fs_stat(&t.console, &t.p("f")).unwrap();
        assert_eq!((s.kind.as_str(), s.size), ("file", 3));
        assert!(ps5upload_core::fs_ops::fs_exists(&t.console, &t.p("f")).unwrap());
        assert!(!ps5upload_core::fs_ops::fs_exists(&t.console, &t.p("nope")).unwrap());
    }

    #[test]
    fn a_cross_device_move_keeps_the_text_the_engine_matches_on() {
        let t = rig("t-xdev", true);
        std::fs::create_dir_all(t.root.join("mnt2")).unwrap();
        std::fs::write(t.root.join("a"), b"1").unwrap();
        mgmt_fs::set(true, 0, 0);
        let e = ps5upload_core::fs_ops::fs_move(&t.console, &t.p("a"), &t.p("mnt2/a")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected FS_MOVE: fs_move_cross_mount"
        );
        assert_eq!(
            e.downcast_ref::<MgmtError>().unwrap().status,
            gen::ERR_CROSS_DEVICE
        );
        assert!(t.root.join("a").exists());
        mgmt_fs::set(false, 0, 0);
        ps5upload_core::fs_ops::fs_move(&t.console, &t.p("a"), &t.p("b")).unwrap();
        assert!(t.root.join("b").exists());
    }

    #[test]
    fn mkdir_chmod_and_the_path_policy_through_core() {
        let t = rig("t-mk", true);
        ps5upload_core::fs_ops::fs_mkdir(&t.console, &t.p("x/y/z")).unwrap();
        assert!(t.root.join("x/y/z").is_dir());
        ps5upload_core::fs_ops::fs_chmod(&t.console, &t.p("x"), "0700", false).unwrap();
        assert_eq!(
            std::fs::metadata(t.root.join("x")).unwrap().mode() & 0o7777,
            0o700
        );
        let e = ps5upload_core::fs_ops::fs_mkdir(&t.console, "/etc/evil").unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected FS_MKDIR: fs_mkdir_path_not_allowed"
        );
    }

    #[test]
    fn log_and_net_calls_through_core() {
        let t = rig("t-log", true);
        mgmt_fs::set(false, 3000, 0);
        let k = ps5upload_core::diagnostics::klog_read(&t.console, 2000).unwrap();
        assert_eq!(k.len(), 2000);
        let n = ps5upload_core::diagnostics::net_interfaces(&t.console).unwrap();
        assert_eq!(n.interfaces.len(), 1);
        // net.reach: an unreachable host is a REPLY (`ok:false` with its fields), not an error
        let r = ps5upload_core::diagnostics::net_reach(&t.console, "unreachable", 9, 100).unwrap();
        assert!(!r.ok && r.timed_out && r.ms == 3000, "{r:?}");
        let r = ps5upload_core::diagnostics::net_reach(&t.console, "10.0.0.1", 9, 100).unwrap();
        assert!(r.ok && r.ms == 4);
        // the speed test is N round trips
        let s = ps5upload_core::diagnostics::net_speed_test(&t.console, 5).unwrap();
        assert_eq!(s.round_trips, 5);
        // a mount failure keeps its code and says so
        let e = ps5upload_core::diagnostics::pkg_direct_mount(&t.console, "/mnt/x.pkg", None)
            .unwrap_err();
        assert!(
            format!("{e:#}").contains("PKG_DIRECT_MOUNT failed"),
            "{e:#}"
        );
        // shutdown (node.cleanup is a job op now, Task 5)
        assert!(ps5upload_core::payload_lifecycle::shutdown_running_payload(&t.console).unwrap());
        assert_eq!(mgmt_fs::stats().1, 1);
    }

    #[test]
    fn a_server_without_the_dispatcher_is_helper_not_ava1_from_its_capability_bit() {
        let t = rig("t-nocap", false);
        // CAP_MGMT is absent (no table installed): the transport refuses with the error the
        // person can act on, and no management request was sent to find that out.
        let r = ps5upload_core::mgmt::m::HW_INFO;
        let e = {
            use ps5upload_core::mgmt::MgmtTransport;
            t.transport
                .call(&t.console, r, "HW_INFO", b"", Duration::from_secs(5))
                .unwrap_err()
        };
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
        assert_eq!(mgmt_fs::stats(), (0, 0, 0));
    }
}

// ---- review round 1: kernel-panic class, policy, tmp-file hygiene ----

fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// DISCRIMINATING: a link on device 1 whose target is on device 2, renamed into a directory on device 2.
/// A stat()-based guard compares 2 with 2, says "same", and rename(2) would move the LINK across devices
/// (the kernel panic); the lstat-based one sees device 1 against 2 and refuses. The devices are injected
/// (a host cannot give us two writable ones), but the rename itself is real: on the host it would SUCCEED,
/// so a guard that lets it through is observable as the link having moved.
#[tokio::test(flavor = "multi_thread")]
async fn a_symlink_source_is_judged_by_its_own_device() {
    let r = rig("xdevlink").await;
    std::fs::create_dir_all(r.path("mnt2")).unwrap();
    std::fs::write(r.path("mnt2/real"), b"r").unwrap();
    symlink(r.path("mnt2/real"), r.path("lnk"));
    mgmt_fs::set_link_devices();
    let (st, b) = r
        .rpc(
            gen::METHOD_FS_RENAME,
            &rename(&r, "lnk", "mnt2/lnk-moved", 1),
        )
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_CROSS_DEVICE, "fs_move_cross_mount")
    );
    assert!(
        std::fs::symlink_metadata(r.path("lnk")).is_ok(),
        "rename(2) was never called: the link is still there"
    );
    assert!(std::fs::symlink_metadata(r.path("mnt2/lnk-moved")).is_err());
    // the same-device control: the link moves inside its own directory (device 1 to device 1)
    mgmt_fs::set_link_devices();
    let (st, _) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "lnk", "lnk-renamed", 1))
        .await;
    assert_eq!(st, OK);
    assert!(std::fs::symlink_metadata(r.path("lnk-renamed"))
        .unwrap()
        .file_type()
        .is_symlink());
}

/// Real lstat and a real link, no injection. It does NOT discriminate by itself (rename(2) across /dev gives
/// EXDEV, which maps to the same status and cause), so it is a smoke test of the real lookups: a link to
/// /dev/null and a plain file are both refused when moved into /dev, EACCES-or-EXDEV never reaching the user as
/// anything but ERR_CROSS_DEVICE, and nothing appears in /dev.
#[tokio::test(flavor = "multi_thread")]
async fn real_lstat_refuses_a_move_into_another_device() {
    let r = rig("xdevreal").await;
    let dev_of = |p: &Path| std::fs::metadata(p).unwrap().dev();
    if dev_of(&r.root) == dev_of(Path::new("/dev")) {
        eprintln!("skipped: the temp root and /dev share a device on this host");
        return;
    }
    mgmt_fs::allow_dev(true);
    symlink("/dev/null", r.path("lnk"));
    std::fs::write(r.path("plain"), b"p").unwrap();
    for from in ["lnk", "plain"] {
        let (st, b) = r
            .rpc(
                gen::METHOD_FS_RENAME,
                &gen::FsRename {
                    from: r.p(from),
                    to: "/dev/ava1-should-never-exist".into(),
                    overwrite: 1,
                },
            )
            .await;
        assert_eq!(
            (st, cause(&b).as_str()),
            (gen::ERR_CROSS_DEVICE, "fs_move_cross_mount"),
            "{from}"
        );
        assert!(std::fs::symlink_metadata(r.path(from)).is_ok());
    }
    assert!(!Path::new("/dev/ava1-should-never-exist").exists());
}

/// LINT (a source check, not behaviour): every caller of the guard passes the lstat lookup for the source.
#[test]
fn lint_the_guard_header_judges_a_source_link_by_lstat() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload/include/cross_device.h"),
    )
    .unwrap();
    assert!(
        src.contains("xdev_lstat_dev"),
        "the lstat device lookup exists"
    );
    // Every caller that renames a user-chosen source passes the lstat lookup for it.
    for f in ["src/ftp_server.c", "src/mgmt_fs.c"] {
        let c = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../payload")
                .join(f),
        )
        .unwrap();
        assert!(
            c.contains("xdev_rename_crosses_l("),
            "{f} uses the two-lookup guard"
        );
        assert!(
            !c.contains("xdev_rename_crosses("),
            "{f} still uses the one-lookup (stat) guard"
        );
    }
    let sh = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload/src/shell_builtin.c"),
    )
    .unwrap();
    assert!(sh.contains("mv_same_dev = (lstat(argv[i]"));
    // Fail closed (review 007 #4): every guard caller refuses anything but a definite SAME.
    for f in ["src/ftp_server.c", "src/mgmt_fs.c"] {
        let c = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../payload")
                .join(f),
        )
        .unwrap();
        assert!(
            c.contains("xdev_rename_is_safe("),
            "{f} must refuse UNKNOWN"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nonexistent_leaf_under_a_symlinked_parent_is_refused_by_the_real_policy() {
    let r = rig("symparent").await;
    let outside = r.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, r.path("link"));
    // fs.write
    let (st, b) = r.write("link/new", 0, 0, b"x", None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_PATH, "path_unsafe"));
    assert!(!outside.join("new").exists());
    // fs.mkdir, with and without parents (the missing parents are created below the link too)
    for rel in ["link/sub", "link/a/b/c"] {
        let (st, _) = r
            .rpc(
                gen::METHOD_FS_MKDIR,
                &gen::FsMkdir {
                    path: r.p(rel),
                    mode: 0o755,
                    parents: 1,
                },
            )
            .await;
        assert_eq!(st, gen::ERR_PATH, "{rel}");
    }
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing was created outside"
    );
    // fs.rename's destination
    std::fs::write(r.path("src"), b"1").unwrap();
    let (st, b) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", "link/dst", 1))
        .await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_move_path_not_allowed")
    );
    assert!(r.path("src").exists() && !outside.join("dst").exists());
    // a dangling symlink leaf is refused too: creating through it would write where it points
    symlink(outside.join("target-not-there"), r.path("dangling"));
    let (st, _) = r.write("dangling", 0, 0, b"x", None).await;
    assert_eq!(st, gen::ERR_PATH);
    assert!(!outside.join("target-not-there").exists());
    // the policy still accepts the honest cases: a missing leaf in a real directory, and a deeper one
    let (st, _) = r.write("ok-leaf", 0, 0, b"x", None).await;
    assert_eq!(st, OK);
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_MKDIR,
            &gen::FsMkdir {
                path: r.p("real/a/b"),
                mode: 0o755,
                parents: 1,
            },
        )
        .await;
    assert_eq!(st, OK);
    // and a symlink INSIDE the root that stays inside is fine
    symlink(r.path("real"), r.path("inlink"));
    let (st, _) = r.write("inlink/file", 0, 0, b"x", None).await;
    assert_eq!(st, OK);
    assert!(r.path("real/file").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_planted_tmp_symlink_is_never_followed() {
    let r = rig("tmplink").await;
    let outside = r.root.parent().unwrap().join("victim");
    std::fs::write(&outside, b"precious").unwrap();
    // single-call write: the planted link at the tmp name is removed, not written through
    symlink(&outside, r.path("f.ps5upload.tmp"));
    let (st, _) = r.write("f", 0, 0, b"mine", None).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(&outside).unwrap(), b"precious");
    assert_eq!(std::fs::read(r.path("f")).unwrap(), b"mine");
    // first chunk: same
    symlink(&outside, r.path("g.ps5upload.tmp"));
    let (st, _) = r.write("g", 0, AT, b"aa", None).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(&outside).unwrap(), b"precious");
    // a link planted BETWEEN chunks: the next chunk is refused, the target untouched
    std::fs::remove_file(r.path("g.ps5upload.tmp")).unwrap();
    symlink(&outside, r.path("g.ps5upload.tmp"));
    let (st, b) = r.write("g", 2, AT | COMMIT, b"bb", None).await;
    assert_eq!((st, cause(&b).as_str()), (gen::ERR_IO, "open_failed"));
    assert_eq!(std::fs::read(&outside).unwrap(), b"precious");
    assert!(!r.path("g").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fifo_cannot_block_a_worker() {
    let r = rig("fifo").await;
    let mk = |p: PathBuf| {
        assert!(std::process::Command::new("mkfifo")
            .arg(&p)
            .status()
            .unwrap()
            .success());
    };
    // fs.read of a FIFO: opened non-blocking, then refused as not a regular file
    mk(r.path("pipe"));
    let (st, b) = tokio::time::timeout(Duration::from_secs(5), r.read("pipe", 0, 10, 0))
        .await
        .expect("fs.read of a FIFO must not block");
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_IO, "fs_read_not_regular_file")
    );
    // a FIFO at the tmp name: a later chunk is refused without blocking
    mk(r.path("w.ps5upload.tmp"));
    let (st, _) = tokio::time::timeout(Duration::from_secs(5), r.write("w", 5, AT, b"x", None))
        .await
        .expect("fs.write onto a FIFO tmp must not block");
    assert_eq!(st, gen::ERR_IO);
    // the single-call write replaces it with a regular file
    let (st, _) = r.write("w", 0, 0, b"ok", None).await;
    assert_eq!(st, OK);
    assert_eq!(std::fs::read(r.path("w")).unwrap(), b"ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_past_zero_without_a_tmp_file_is_refused_not_made_sparse() {
    let r = rig("nosparse").await;
    let (st, b) = r.write("s", 4096, AT, b"tail", None).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PROTOCOL, "fs_write_no_tmp_file")
    );
    assert!(!r.path("s.ps5upload.tmp").exists() && !r.path("s").exists());
    let (st, _) = r.write("s", 4096, AT | COMMIT, b"", None).await;
    assert_eq!(
        st,
        gen::ERR_PROTOCOL,
        "a commit with no tmp file is refused too"
    );
}

// ---- review S2: the trust store (<root>/d/ava stands for /data/ps5upload/ava) is untouchable ----

fn trust_store(r: &Rig) {
    std::fs::create_dir_all(r.path("d/ava")).unwrap();
    std::fs::write(r.path("d/ava/peers"), b"trusted").unwrap();
    std::fs::write(r.path("d/ava/identity"), b"secret").unwrap();
}

/// review S2: write, read, rename (into and out of), mkdir, chmod and stat-free probes under the
/// directory are all refused through fs.* over AVA1, and nothing changed on disk.
#[tokio::test(flavor = "multi_thread")]
async fn s2_the_trust_store_refuses_every_fs_method() {
    let r = rig("s2fs").await;
    trust_store(&r);
    std::fs::write(r.path("src"), b"mine").unwrap();
    // write: over the peers file, and a new file
    for rel in ["d/ava/peers", "d/ava/new", "d/ava/identity"] {
        let (st, _) = r.write(rel, 0, 0, b"evil", None).await;
        assert_eq!(st, gen::ERR_PATH, "write {rel}");
        let (st, _) = r.write(rel, 0, AT, b"evil", None).await;
        assert_eq!(st, gen::ERR_PATH, "chunk {rel}");
    }
    assert_eq!(std::fs::read(r.path("d/ava/peers")).unwrap(), b"trusted");
    assert!(!r.path("d/ava/new").exists() && !r.path("d/ava/peers.ps5upload.tmp").exists());
    // read
    let (st, b) = r.read("d/ava/identity", 0, 100, 0).await;
    assert_eq!(
        (st, cause(&b).as_str()),
        (gen::ERR_PATH, "fs_read_path_not_allowed")
    );
    let (st, _) = r.read("d/ava/identity", 0, 100, gen::FSR_UNSAFE).await;
    assert_eq!(st, gen::ERR_PATH);
    // rename into, and out of
    let (st, _) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "src", "d/ava/peers", 1))
        .await;
    assert_eq!(st, gen::ERR_PATH);
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_RENAME,
            &rename(&r, "d/ava/peers", "stolen", 1),
        )
        .await;
    assert_eq!(st, gen::ERR_PATH);
    assert_eq!(std::fs::read(r.path("d/ava/peers")).unwrap(), b"trusted");
    // rename of an ANCESTOR (takes the store with it)
    let (st, _) = r
        .rpc(gen::METHOD_FS_RENAME, &rename(&r, "d", "d2", 1))
        .await;
    assert_eq!(st, gen::ERR_PATH);
    assert!(r.path("d/ava").exists());
    // mkdir under it, with parents
    for rel in ["d/ava/sub", "d/ava/x/y/z"] {
        let (st, _) = r
            .rpc(
                gen::METHOD_FS_MKDIR,
                &gen::FsMkdir {
                    path: r.p(rel),
                    mode: 0o755,
                    parents: 1,
                },
            )
            .await;
        assert_eq!(st, gen::ERR_PATH, "mkdir {rel}");
    }
    // chmod
    let (st, _) = r
        .rpc(
            gen::METHOD_FS_CHMOD,
            &gen::FsChmod {
                path: r.p("d/ava/peers"),
                mode: 0o777,
            },
        )
        .await;
    assert_eq!(st, gen::ERR_PATH);
    // its siblings are still ordinary
    let (st, _) = r.write("d/other", 0, 0, b"fine", None).await;
    assert_eq!(st, OK);
}

/// review S2: every spelling of the path: `//`, `.`, `..`, a symlink to it, a symlink into it,
/// a case variant; and a symlink that points at it from outside.
#[tokio::test(flavor = "multi_thread")]
async fn s2_the_trust_store_is_found_through_every_spelling() {
    let r = rig("s2spell").await;
    trust_store(&r);
    let root = r.root.display().to_string();
    symlink(r.path("d/ava"), r.path("tolink"));
    symlink(r.path("d"), r.path("todir"));
    let spellings = [
        format!("{root}/d/ava/peers"),
        format!("{root}//d//ava//peers"),
        format!("{root}/d/./ava/peers"),
        format!("{root}/d/x/../ava/peers"),
        format!("{root}/D/AVA/Peers"),
        format!("{root}/d/Ava/peers"),
        format!("{root}/tolink/peers"),
        format!("{root}/tolink/brand-new"),
        format!("{root}/todir/ava/peers"),
        format!("{root}/todir/ava/brand-new"),
        format!("{root}/d/ava"),
    ];
    for p in &spellings {
        assert!(mgmt_fs::in_protected(p), "{p} must be protected");
        let (st, _) = r
            .rpc(
                gen::METHOD_FS_WRITE,
                &FsWrite {
                    path: p.clone(),
                    offset: 0,
                    flags: 0,
                    data: b"evil".to_vec(),
                    mode: None,
                },
            )
            .await;
        // the lexical `.`/`..` forms and everything else: refused (ERR_PATH), never written
        assert_eq!(st, gen::ERR_PATH, "write {p}");
    }
    assert_eq!(std::fs::read(r.path("d/ava/peers")).unwrap(), b"trusted");
    assert!(!r.path("d/ava/brand-new").exists());
    // ancestors and delete-style questions (a destructive op on a source path asks `contains`)
    for p in [
        format!("{root}/d"),
        format!("{root}/d/"),
        format!("{root}/d/ava"),
        format!("{root}/todir"),
    ] {
        assert!(mgmt_fs::contains_protected(&p), "{p} contains the store");
    }
    for p in [
        format!("{root}/other"),
        format!("{root}/d/ava2"),
        format!("{root}/dd"),
    ] {
        assert!(
            !mgmt_fs::in_protected(&p) && !mgmt_fs::contains_protected(&p),
            "{p} is unrelated"
        );
    }
    // a link planted elsewhere that points at it is refused to rename/write through too
    symlink(r.path("d/ava/peers"), r.path("plant"));
    let (st, _) = r.write("plant", 0, 0, b"evil", None).await;
    assert_eq!(st, gen::ERR_PATH);
    assert_eq!(std::fs::read(r.path("d/ava/peers")).unwrap(), b"trusted");
}

// ---- review S2, round 2: ancestors and recursive operations ----

/// LINT (a source check, not a behavioural test: ava1_glue.c is SDK-only and cannot be built on the host). Every entry point that walks a tree, or that moves/replaces one, refuses a path that
/// is the trust store or an ancestor of it through the shared `path_tree_op_refused`. The behaviour of that
/// function itself is pinned by `s2_tree_op_refusal_covers_ancestors_and_the_store`, the FTP handlers by
/// payload/tests/ftp_trust_store_selftest.c (run by the root Makefile's payload selftests and by
/// `s2_ftp_selftest_passes` below).
#[test]
fn s2_lint_recursive_entry_points_use_the_shared_refusal() {
    let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload");
    // The old per-handler checks in runtime.c went with the handlers (the AVA1 paths replaced them): fs.rename
    // is native (mgmt_fs.c), the tree jobs (delete, chmod -R, hash, crc32) are in fs_jobs.c behind may_write, and
    // copy / upload / download are the data layer's hooks below.
    let src = |f: &str| std::fs::read_to_string(payload.join(f)).unwrap();
    for f in ["src/mgmt_fs.c", "src/fs_jobs.c"] {
        assert!(
            src(f).contains("path_tree_op_refused("),
            "{f} must refuse trust-store ancestors"
        );
    }
    // round 3: every file the data layer writes is opened O_NOFOLLOW (a planted link is never written through)
    assert!(
        src("ava1/ava1_apply.c").contains("O_CREAT | O_TRUNC | O_NOFOLLOW"),
        "the apply path must not open its destination through a link"
    );
    let glue = std::fs::read_to_string(payload.join("src/ava1_glue.c")).unwrap();
    assert!(
        glue.contains("dc.refuse_link = path_tree_op_refused"),
        "the data layer refuses links into the store"
    );
    assert!(
        glue.contains("path_tree_op_refused("),
        "the AVA1 data-plane hooks must too (copy, upload root, download root)"
    );
}

/// The shared refusal: the store, anything below it AND every ancestor are refused; siblings are not.
#[test]
fn s2_tree_op_refusal_covers_ancestors_and_the_store() {
    let _g = one();
    let base = std::env::temp_dir().join(format!("ava1-s2-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("d/ava")).unwrap();
    let base = base.canonicalize().unwrap();
    assert_eq!(mgmt_fs::install(&base), 0);
    let p = |s: &str| format!("{}/{s}", base.display());
    for refused in ["d/ava", "d/ava/peers", "d", "", "d/ava/new/deeper"] {
        assert!(
            mgmt_fs::tree_op_refused(&p(refused)),
            "{refused:?} must be refused"
        );
    }
    assert!(mgmt_fs::tree_op_refused("/"), "the root is an ancestor");
    for ok in ["other", "d/other", "d/ava2", "dd"] {
        assert!(!mgmt_fs::tree_op_refused(&p(ok)), "{ok} is fine");
    }
    mgmt_fs::uninstall();
}

#[test]
fn s2_ftp_selftest_passes() {
    let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload");
    let exe = std::env::temp_dir().join(format!("ava1-ftp-s2-{}", std::process::id()));
    let cc = std::process::Command::new("cc")
        .args(["-O2", "-Wall", "-Wextra", "-Werror", "-pthread", "-I"])
        .arg(payload.join("include"))
        .arg("-o")
        .arg(&exe)
        .arg(payload.join("tests/ftp_trust_store_selftest.c"))
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = std::process::Command::new(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Review S2 round 3, item 3: `is_safe_unsafe_read_path` already rejects `..` (runtime.c, checked), and the
/// trust-store check itself fails closed on a `..` component, so `link/../x` cannot be judged differently
/// from what the kernel resolves.
#[test]
fn s2_a_dotdot_path_fails_closed() {
    let _g = one();
    let base = std::env::temp_dir().join(format!("ava1-s2-dotdot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("d/ava")).unwrap();
    let base = base.canonicalize().unwrap();
    assert_eq!(mgmt_fs::install(&base), 0);
    for p in [
        format!("{}/elsewhere/../d/ava/peers", base.display()),
        format!("{}/link/../harmless", base.display()),
    ] {
        assert!(
            mgmt_fs::in_protected(&p) && mgmt_fs::tree_op_refused(&p),
            "{p}"
        );
    }
    let rt = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload/src/runtime.c"),
    )
    .unwrap();
    let a = rt
        .find("int is_safe_unsafe_read_path(const char *p) {")
        .unwrap();
    assert!(
        rt[a..a + 300].contains("path_has_dotdot_component(p)"),
        "the unsafe-read check rejects `..`"
    );
    mgmt_fs::uninstall();
}
