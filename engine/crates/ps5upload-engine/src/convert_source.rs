//! Convert reading a game in place: the converter's opener for `remote://` (a saved server,
//! over the same server file system — read-ahead and retries — uploads use) and `ps5://`
//! (the console, through AVA1 management reads: see `console_read`).

use std::path::Path;
use std::sync::Arc;

use ps5upload_core::source_fs::SourceFs;
use ps5upload_fpkg::remote_source::{self, RemoteFiles};
use ps5upload_fpkg::ReadSeek;

use crate::remote::source_fs::RemoteSourceFs;

/// Let `source::open` (build, estimate, inspect, the package viewer) read `remote://` paths.
pub(crate) fn register() {
    remote_source::register(Box::new(open));
}

fn open(url: &str) -> ps5upload_fpkg::Result<(Arc<dyn RemoteFiles>, String)> {
    let fail = |m: String| ps5upload_fpkg::Error::Format(m);
    // Converter work runs on blocking threads, which can wait on the runtime.
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| fail(format!("{url}: no runtime to reach the server from")))?;
    if url.starts_with("ps5://") {
        return open_console(&handle, url);
    }
    let (fs, path) = handle
        .block_on(RemoteSourceFs::for_path(url))
        .map_err(|e| fail(format!("{url}: {e}")))?;
    let label = format!("server {}", connection_of(url));
    Ok((
        Arc::new(ServerFiles { fs, label }),
        path.to_string_lossy().into_owned(),
    ))
}

fn connection_of(url: &str) -> &str {
    let rest = url.strip_prefix("remote://").unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// `ps5://192.168.1.5/mnt/ext0/games/X.exfat` → ("192.168.1.5", "/mnt/ext0/games/X.exfat").
/// An installed game is refused: its files are encrypted, so only a dump converts.
fn console_target(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("ps5://")
        .ok_or_else(|| format!("{url} is not a ps5:// path"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        return Err(format!("{url} names no console"));
    }
    let path = format!("/{path}");
    let installed = ["/user/app/", "/system_ex/app/", "/user/patch/"];
    if installed
        .iter()
        .any(|p| path.starts_with(p) || path == p.trim_end_matches('/'))
    {
        return Err(format!(
            "{path} is an installed game, and installed games are encrypted; convert a dump (a game folder or image) instead"
        ));
    }
    Ok((host.to_string(), path))
}

fn open_console(
    _handle: &tokio::runtime::Handle,
    url: &str,
) -> ps5upload_fpkg::Result<(Arc<dyn RemoteFiles>, String)> {
    let (host, path) = console_target(url).map_err(ps5upload_fpkg::Error::Format)?;
    // Nothing is dialled here: the first stat or read opens (or reuses) the AVA1 session, and
    // an unreachable or unpaired console fails there with the same message an upload gives.
    Ok((
        Arc::new(crate::console_read::ConsoleFiles {
            addr: crate::console_addr(&host),
            label: format!("the PS5 at {host}"),
        }),
        path,
    ))
}

struct ServerFiles {
    fs: Arc<RemoteSourceFs>,
    label: String,
}

impl RemoteFiles for ServerFiles {
    fn open(&self, path: &str) -> std::io::Result<Box<dyn ReadSeek>> {
        let r = self.fs.open(Path::new(path))?;
        Ok(Box::new(r))
    }

    fn stat(&self, path: &str) -> std::io::Result<(u64, bool)> {
        let m = self.fs.metadata(Path::new(path))?;
        Ok((m.len, m.is_dir))
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>> {
        self.fs
            .read_dir(Path::new(dir))?
            .into_iter()
            .map(|(child, is_dir)| {
                // The listing already said the size; this answers from its cache.
                let size = if is_dir {
                    0
                } else {
                    self.fs.metadata(&child)?.len
                };
                let name = child
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok((name, is_dir, size))
            })
            .collect()
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A game image on a saved server converts without a copy here: the converter reads it
    /// through the server file system.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_game_image_on_a_server_opens_and_builds_in_place() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ps5upload-fpkg/tests/fixtures/mini.exfat");
        let bytes = std::fs::read(&fixture).unwrap();
        let r = crate::remote::pool::testing::install_global(&[
            ("/fpkg-remote/mini.exfat", &bytes),
            ("/fpkg-remote/game/sce_sys/param.json", b"{}"),
        ]);
        let id = r
            .store
            .add(
                crate::remote::store::conn("NAS", crate::remote::store::Protocol::Smb),
                crate::remote::store::Secret::None,
            )
            .unwrap()
            .conn
            .id;
        register();
        let url = format!("remote://{id}/fpkg-remote/mini.exfat");
        let out = std::env::temp_dir().join(format!("fpkg-remote-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let (u, o) = (url.clone(), out.clone());
        let report = tokio::task::spawn_blocking(move || {
            let local = ps5upload_fpkg::source::open(&fixture).unwrap();
            let remote = ps5upload_fpkg::source::open(Path::new(&u)).unwrap();
            assert_eq!(local.files(), remote.files());
            assert!(
                remote.describe().contains("server"),
                "{}",
                remote.describe()
            );
            let request = ps5upload_fpkg::build::BuildRequest {
                kraken: false,
                ..ps5upload_fpkg::build::BuildRequest::new(Path::new(&u), &o)
            };
            ps5upload_fpkg::build::build(&request, &mut |_| {})
        })
        .await
        .unwrap()
        .unwrap();
        assert!(report.verify.ok(), "{}", report.verify);
        let folder = format!("remote://{id}/fpkg-remote/game");
        let tree = tokio::task::spawn_blocking(move || {
            ps5upload_fpkg::source::open(Path::new(&folder)).map(|t| t.files().to_vec())
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].path, "sce_sys/param.json");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn a_console_path_names_the_host_and_refuses_installed_games() {
        assert_eq!(
            console_target("ps5://192.168.86.99/mnt/ext0/g/X.exfat").unwrap(),
            (
                "192.168.86.99".to_string(),
                "/mnt/ext0/g/X.exfat".to_string()
            )
        );
        assert_eq!(
            console_target("ps5://10.0.0.2:8080/data/homebrew/G")
                .unwrap()
                .0,
            "10.0.0.2"
        );
        for p in [
            "ps5://h/user/app/PPSA01234",
            "ps5://h/user/app",
            "ps5://h/system_ex/app/X",
        ] {
            let e = console_target(p).unwrap_err();
            assert!(e.contains("encrypted"), "{p}: {e}");
        }
        assert!(console_target("ps5:///x").is_err());
    }

    #[test]
    fn a_remote_path_is_not_made_relative_to_the_engine() {
        assert_eq!(
            crate::fpkg_api::resolve_engine_path("ps5://h/data/x"),
            std::path::PathBuf::from("ps5://h/data/x")
        );
        assert_eq!(
            crate::fpkg_api::resolve_engine_path("remote://abc/games/x"),
            std::path::PathBuf::from("remote://abc/games/x")
        );
    }

    /// A console as the management plane answers for it: `fs.stat`, `fs.list` and `fs.read`
    /// over an in-memory tree. Nothing listens on any FTP port anywhere in these tests.
    struct FakeConsole {
        files: std::collections::BTreeMap<String, Vec<u8>>,
        calls: std::sync::Mutex<Vec<(String, u16)>>,
    }

    impl FakeConsole {
        fn new(files: &[(&str, &[u8])]) -> Arc<Self> {
            Arc::new(Self {
                files: files
                    .iter()
                    .map(|(p, b)| (p.to_string(), b.to_vec()))
                    .collect(),
                calls: Default::default(),
            })
        }

        fn is_dir(&self, p: &str) -> bool {
            let d = format!("{}/", p.trim_end_matches('/'));
            self.files.keys().any(|k| k.starts_with(&d))
        }
    }

    impl ps5upload_core::mgmt::MgmtTransport for FakeConsole {
        fn call(
            &self,
            addr: &str,
            method: ps5upload_core::mgmt::Method,
            label: &str,
            body: &[u8],
            _t: std::time::Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            use ps5upload_core::mgmt::m;
            self.calls.lock().unwrap().push((addr.into(), method.id));
            let v: serde_json::Value = serde_json::from_slice(body).unwrap();
            let path = v["path"].as_str().unwrap_or("").to_string();
            let missing = || anyhow::anyhow!("payload rejected {label}: fs_stat_failed_errno_2");
            let out = if method.id == m::FS_STAT.id {
                if let Some(b) = self.files.get(&path) {
                    serde_json::json!({"kind":"file","size":b.len(),"mtime":0,"mode":420,"dev":1})
                } else if self.is_dir(&path) {
                    serde_json::json!({"kind":"dir","size":0,"mtime":0,"mode":493,"dev":1})
                } else {
                    return Err(missing());
                }
            } else if method.id == m::FS_READ.id {
                let b = self.files.get(&path).ok_or_else(missing)?;
                let off = (v["offset"].as_u64().unwrap() as usize).min(b.len());
                let end = (off + (v["limit"].as_u64().unwrap() as usize).min(2 << 20)).min(b.len());
                return Ok(Some(b[off..end].to_vec()));
            } else if method.id == m::FS_LIST.id {
                let prefix = format!("{}/", path.trim_end_matches('/'));
                let mut names = std::collections::BTreeMap::new();
                for (k, b) in &self.files {
                    if let Some(rest) = k.strip_prefix(&prefix) {
                        match rest.split_once('/') {
                            Some((d, _)) => names.insert(d.to_string(), ("dir", 0)),
                            None => names.insert(rest.to_string(), ("file", b.len())),
                        };
                    }
                }
                let off = v["offset"].as_u64().unwrap() as usize;
                let lim = v["limit"].as_u64().unwrap() as usize;
                let all: Vec<_> = names.into_iter().collect();
                let page: Vec<_> = all
                    .iter()
                    .skip(off)
                    .take(lim)
                    .map(|(n, (k, s))| serde_json::json!({"name":n,"kind":k,"size":s,"mtime":0}))
                    .collect();
                serde_json::json!({
                    "path": path, "entries": page, "truncated": false,
                    "total_scanned": all.len(), "returned": page.len()
                })
            } else {
                anyhow::bail!("unexpected method {}", method.id);
            };
            Ok(Some(serde_json::to_vec(&out).unwrap()))
        }
    }

    /// The issue #351 repro on AVA1: a game image on the console converts in place, with every
    /// read an AVA1 management call (and so no console FTP server).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_console_game_image_inspects_and_builds_over_management_reads() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ps5upload-fpkg/tests/fixtures/mini.exfat");
        let bytes = std::fs::read(&fixture).unwrap();
        let console = FakeConsole::new(&[
            ("/data/games/mini.exfat", &bytes),
            ("/data/games/folder/sce_sys/param.json", b"{}"),
        ]);
        register();
        let url = "ps5://192.168.86.99/data/games/mini.exfat".to_string();
        let out = std::env::temp_dir().join(format!("fpkg-console-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let (c, o) = (console.clone(), out.clone());
        let report = tokio::task::spawn_blocking(move || {
            let _g = ps5upload_core::mgmt::scoped_transport(c);
            let local = ps5upload_fpkg::source::open(&fixture).unwrap();
            let remote = ps5upload_fpkg::source::open(Path::new(&url)).unwrap();
            assert_eq!(local.files(), remote.files());
            assert!(
                remote.describe().contains("the PS5 at 192.168.86.99"),
                "{}",
                remote.describe()
            );
            let request = ps5upload_fpkg::build::BuildRequest {
                kraken: false,
                ..ps5upload_fpkg::build::BuildRequest::new(Path::new(&url), &o)
            };
            ps5upload_fpkg::build::build(&request, &mut |_| {})
        })
        .await
        .unwrap()
        .unwrap();
        assert!(report.verify.ok(), "{}", report.verify);
        // Only management calls reached the console, addressed by host alone.
        let calls = console.calls.lock().unwrap();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|(a, _)| a == "192.168.86.99"));
        let _ = std::fs::remove_dir_all(&out);
    }

    /// A game folder on the console lists and reads over management calls too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_console_game_folder_lists_over_management_calls() {
        let console = FakeConsole::new(&[
            ("/data/games/folder/sce_sys/param.json", b"{}"),
            ("/data/games/folder/eboot.bin", b"abcd"),
        ]);
        register();
        let tree = tokio::task::spawn_blocking(move || {
            let _g = ps5upload_core::mgmt::scoped_transport(console);
            ps5upload_fpkg::source::open(Path::new("ps5://10.0.0.5:8080/data/games/folder"))
                .map(|t| t.files().to_vec())
        })
        .await
        .unwrap()
        .unwrap();
        let mut names: Vec<_> = tree.iter().map(|f| f.path.clone()).collect();
        names.sort();
        assert_eq!(names, ["eboot.bin", "sce_sys/param.json"]);
    }

    /// A missing path is a typed not-found, and an unreachable console says so in the
    /// helper/pairing words, never a question about an FTP server.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_console_path_is_not_found_and_an_unreachable_console_says_why() {
        let console = FakeConsole::new(&[("/data/a", b"x")]);
        let files = crate::console_read::ConsoleFiles {
            addr: "h".into(),
            label: "the PS5 at h".into(),
        };
        let (missing, unreachable) = tokio::task::spawn_blocking(move || {
            let missing = {
                let _g = ps5upload_core::mgmt::scoped_transport(console);
                files.open("/data/nope").err().unwrap().kind()
            };
            struct Down;
            impl ps5upload_core::mgmt::MgmtTransport for Down {
                fn call(
                    &self,
                    _: &str,
                    _: ps5upload_core::mgmt::Method,
                    label: &str,
                    _: &[u8],
                    _: std::time::Duration,
                ) -> anyhow::Result<Option<Vec<u8>>> {
                    Err(ps5upload_core::mgmt::helper_not_ava1(label))
                }
            }
            let _g = ps5upload_core::mgmt::scoped_transport(Arc::new(Down));
            let e = files.stat("/data/a").unwrap_err().to_string();
            (missing, e)
        })
        .await
        .unwrap();
        assert_eq!(missing, std::io::ErrorKind::NotFound);
        assert!(unreachable.contains("helper_not_ava1"), "{unreachable}");
        assert!(!unreachable.contains("FTP"), "{unreachable}");
    }

    /// Throughput of the console reader Convert uses, on a real file.
    /// Run by hand against a paired console:
    /// PS5UPLOAD_SPEED_HOST=192.168.86.99 PS5UPLOAD_SPEED_FILE=/user/app/X/app.pkg
    /// cargo test -p ps5upload-engine --release --lib console_read_speed -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn console_read_speed() {
        use std::io::Read;
        let host = std::env::var("PS5UPLOAD_SPEED_HOST").expect("PS5UPLOAD_SPEED_HOST");
        let path = std::env::var("PS5UPLOAD_SPEED_FILE").expect("PS5UPLOAD_SPEED_FILE");
        let limit: u64 = std::env::var("PS5UPLOAD_SPEED_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2 << 30);
        ps5upload_ava1::mgmt::install();
        tokio::task::spawn_blocking(move || {
            let files = crate::console_read::ConsoleFiles {
                addr: crate::console_addr(&host),
                label: "speed".into(),
            };
            let mut r = files.open(&path).unwrap();
            let mut buf = vec![0u8; 2 << 20];
            let started = std::time::Instant::now();
            let mut done = 0u64;
            while done < limit {
                let n = r.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                done += n as u64;
            }
            let secs = started.elapsed().as_secs_f64();
            println!(
                "console read: {:.2} GiB in {:.1} s = {:.1} MB/s",
                done as f64 / (1u64 << 30) as f64,
                secs,
                done as f64 / 1e6 / secs
            );
        })
        .await
        .unwrap();
    }

    /// The #351 repro, live: inspect and build a console game dump (a folder or an image) with
    /// the console's FTP server stopped. Pair first (see `ps5upload-tests/tests/ava1_live.rs`);
    /// do not run while the engine is connected to the same console.
    /// REAL_PS5_ADDR=192.168.86.99 PS5UPLOAD_CONVERT_PATH=/data/games/X.exfat
    /// cargo test -p ps5upload-engine --release --lib console_game_converts_live -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn console_game_converts_live() {
        let host = std::env::var("REAL_PS5_ADDR").expect("REAL_PS5_ADDR");
        let path = std::env::var("PS5UPLOAD_CONVERT_PATH").expect("PS5UPLOAD_CONVERT_PATH");
        ps5upload_ava1::mgmt::install();
        register();
        let url = format!("ps5://{host}{path}");
        let out = std::env::temp_dir().join(format!("fpkg-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let o = out.clone();
        let started = std::time::Instant::now();
        let report = tokio::task::spawn_blocking(move || {
            let src = ps5upload_fpkg::source::open(Path::new(&url)).unwrap();
            println!("{} ({} files)", src.describe(), src.files().len());
            let request = ps5upload_fpkg::build::BuildRequest {
                kraken: false,
                ..ps5upload_fpkg::build::BuildRequest::new(Path::new(&url), &o)
            };
            ps5upload_fpkg::build::build(&request, &mut |_| {})
        })
        .await
        .unwrap()
        .unwrap();
        assert!(report.verify.ok(), "{}", report.verify);
        println!("converted in {:.1} s", started.elapsed().as_secs_f64());
        let _ = std::fs::remove_dir_all(&out);
    }
}
