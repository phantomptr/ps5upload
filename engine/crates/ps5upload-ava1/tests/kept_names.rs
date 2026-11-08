//! #729: console file names this computer can't hold (`options\gabindings.xml` in a Red Dead
//! Redemption 2 save), downloaded under kept names and uploaded back under the exact ones,
//! against Task 17's `FolderHost` as the console. The console side needs names with '\' and
//! ':', which only a Unix file system holds.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::download::{self, Counters};
use ps5upload_ava1::gen;
use ps5upload_ava1::Pool;
use ps5upload_core::download::DownloadKind;
use ps5upload_core::transfer::{host_unsafe_component, TransferConfig};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-kept-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn within<T>(secs: u64, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("timed out")
}

fn bytes_for(i: usize, n: usize) -> Vec<u8> {
    let mut b = (i as u8).wrapping_mul(37).wrapping_add(11);
    (0..n)
        .map(|_| {
            b = b.wrapping_mul(31).wrapping_add(7);
            b
        })
        .collect()
}

/// A console serving `dir/share`, and a pool it already trusts.
async fn host(dir: &Path) -> Arc<Pool> {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let rpc: ava1::server::RpcHandler = Box::new(|method, _| {
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
    });
    let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc).with_jobs(
        Arc::new(FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    Arc::new(Pool::new(ava).with_addr(addr))
}

fn counters() -> Counters {
    Counters {
        bytes: Arc::default(),
        files: Arc::default(),
        files_finalized: Arc::default(),
        bytes_finalized: Arc::default(),
        total: Some(Arc::default()),
    }
}

/// Every entry under `root` (a file with its bytes, a folder as `None`), by relative path,
/// each name exactly as the file system holds it.
fn names_of(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_str().unwrap().to_string())
                .collect::<Vec<_>>()
                .join("/");
            if p.is_dir() {
                out.insert(rel, None);
                go(root, &p, out);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    if root.exists() {
        go(root, root, &mut out);
    }
    out
}

async fn download(
    pool: Arc<Pool>,
    src: &str,
    dest: &Path,
    escape_names: bool,
    id: u8,
) -> anyhow::Result<u64> {
    let (src, dest) = (src.to_string(), dest.to_path_buf());
    within(
        90,
        tokio::task::spawn_blocking(move || {
            download::to_local_with_in(
                &pool,
                "console",
                &src,
                DownloadKind::Folder,
                &dest,
                false,
                escape_names,
                [id; 16],
                &counters(),
                None,
            )
        }),
    )
    .await
    .unwrap()
}

async fn upload_kept(pool: Arc<Pool>, src: &Path, dest_root: &str, id: u8) -> anyhow::Result<()> {
    let (src, dest_root) = (src.to_path_buf(), dest_root.to_string());
    let mut cfg = TransferConfig::new("127.0.0.1");
    cfg.unescape_names = true;
    within(
        90,
        tokio::task::spawn_blocking(move || {
            ps5upload_ava1::upload::upload_dir_in(&pool, &cfg, [id; 16], &dest_root, &src)
                .map(|_| ())
        }),
    )
    .await
    .unwrap()
}

fn write_tree(root: &Path, files: &[&str]) {
    for (i, name) in files.iter().enumerate() {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes_for(i, 1_000 + i * 997)).unwrap();
    }
}

/// A save as Red Dead Redemption 2 keeps it, plus names no Windows folder can hold and two that
/// would climb out if a '\' were a separator. A plain download refuses it (as before); a kept
/// one lands under names safe on every computer, inside its folder, and goes back to the
/// console under the exact names, byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn a_save_with_names_windows_refuses_round_trips_exactly() {
    let d = temp("round-trip");
    let save = d.join("share/PPSA30528");
    write_tree(
        &save,
        &[
            "options\\gabindings.xml",
            "profile:1.dat",
            "CON",
            "aux.txt",
            "trailing.",
            "RDR2SAVE1.SAV",
            "100%.txt",
            "sce_sys/param.sfo",
            "slot\\2/inner:x",
            "..\\..\\x",
            "a\\..\\..\\b",
        ],
    );
    let pool = host(&d).await;
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let e = download(pool.clone(), "PPSA30528", &out, false, 1)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("backslash"), "{e:#}");
    assert!(
        names_of(&out).is_empty(),
        "a refused download leaves nothing"
    );

    download(pool.clone(), "PPSA30528", &out, true, 2)
        .await
        .unwrap();
    let kept = names_of(&out.join("PPSA30528"));
    assert_eq!(kept.len(), names_of(&save).len());
    for name in kept.keys() {
        for comp in name.split('/') {
            assert!(!comp.contains(['\\', ':']), "{comp:?}");
            assert!(host_unsafe_component(comp).is_none(), "{comp:?}");
        }
    }
    assert!(
        kept.contains_key("options\u{F05C}gabindings.xml"),
        "{kept:?}"
    );
    assert!(kept.contains_key("..\u{F05C}..\u{F05C}x"), "{kept:?}");
    assert!(
        kept.contains_key("a\u{F05C}..\u{F05C}..\u{F05C}b"),
        "{kept:?}"
    );
    // Nothing climbed out: the download folder holds only the save's own folder.
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        1,
        "{:?}",
        names_of(&out)
    );
    assert!(!d.join("x").exists() && !d.join("b").exists());

    upload_kept(pool, &out.join("PPSA30528"), "back", 3)
        .await
        .unwrap();
    assert_eq!(names_of(&d.join("share/back")), names_of(&save));
}

/// A folder of ordinary names (every save kept before #729) goes back unchanged: a '%' or a
/// name that looks like an escape is not read as one.
#[tokio::test(flavor = "multi_thread")]
async fn an_old_backup_folder_uploads_unchanged() {
    let d = temp("old");
    let old = d.join("old/PPSA01650");
    write_tree(
        &old,
        &["sdimg_SAVEDATA00", "100%.txt", "%5C", "sce_sys/param.sfo"],
    );
    std::fs::create_dir_all(d.join("share")).unwrap();
    let pool = host(&d).await;
    upload_kept(pool, &old, "back", 4).await.unwrap();
    assert_eq!(names_of(&d.join("share/back")), names_of(&old));
}

/// A kept name this app would not have made (a letter mapped for no reason, one standing for
/// `..` or holding a '/') is refused before anything reaches the console.
#[tokio::test(flavor = "multi_thread")]
async fn a_kept_name_the_app_did_not_make_is_refused() {
    for (i, bad) in ["\u{F041}bc", "\u{F02E}\u{F02E}", "a\u{F02F}b"]
        .iter()
        .enumerate()
    {
        let d = temp(&format!("bad-{i}"));
        let src = d.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(bad), b"x").unwrap();
        std::fs::create_dir_all(d.join("share")).unwrap();
        let pool = host(&d).await;
        let e = upload_kept(pool, &src, "back", 5 + i as u8)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("not"), "{bad:?}: {e:#}");
        assert!(names_of(&d.join("share")).is_empty(), "{bad:?}");
    }
}
