//! Convert reading a game in place: the converter's opener for `remote://` (a saved server,
//! over the same server file system — read-ahead and retries — uploads use) and `ps5://`
//! (the console, through our own helper's management port: the same FS_READ frames
//! Download uses, so no FTP server has to be running on the console).

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
    if url.starts_with("ps5://") {
        return open_console(url);
    }
    let fail = |m: String| ps5upload_fpkg::Error::Format(m);
    // Converter work runs on blocking threads, which can wait on the runtime.
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| fail(format!("{url}: no runtime to reach the server from")))?;
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

fn open_console(url: &str) -> ps5upload_fpkg::Result<(Arc<dyn RemoteFiles>, String)> {
    let fail = |m: String| ps5upload_fpkg::Error::Format(m);
    let (host, path) = console_target(url).map_err(fail)?;
    let files = HelperFiles::new(&host);
    match files.stat(&path) {
        Ok(_) => Ok((Arc::new(files), path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(fail(format!("{path} isn't on the PS5 at {host}")))
        }
        Err(e) => Err(fail(format!(
            "can't read the console's files at {host} ({e}); is the ps5upload helper running? \
             Send it from the Connection screen and try again."
        ))),
    }
}

/// A directory's children as `(name, is_dir, size)`.
type Listing = Vec<(String, bool, u64)>;

/// The console's files through the helper's management port: no ftpsrv needed. The
/// helper answers one read of at most 2 MiB per connection, so a sequential reader
/// fetches several of those in parallel.
struct HelperFiles {
    mgmt: String,
    label: String,
    /// Directory listings, so a folder's `stat` per file doesn't relist its parent.
    listings: std::sync::Mutex<std::collections::HashMap<String, Listing>>,
}

impl HelperFiles {
    fn new(host: &str) -> Self {
        HelperFiles {
            mgmt: format!("{host}:{}", crate::PS5_MGMT_PORT),
            label: format!("the PS5 at {host} (through the helper)"),
            listings: Default::default(),
        }
    }

    fn cached_list(&self, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>> {
        if let Some(l) = self.listings.lock().unwrap().get(dir) {
            return Ok(l.clone());
        }
        let l = helper_list(&self.mgmt, dir)?;
        self.listings
            .lock()
            .unwrap()
            .insert(dir.to_string(), l.clone());
        Ok(l)
    }
}

fn helper_io(path: &str, e: anyhow::Error) -> std::io::Error {
    let msg = format!("{e:#}");
    let lower = msg.to_ascii_lowercase();
    // The helper names the errno: `fs_list_dir_opendir_errno_2` (ENOENT), `_errno_20` (ENOTDIR).
    let errno_missing = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|t| t.ends_with("errno_2") || t.ends_with("errno_20"));
    let kind = if errno_missing || lower.contains("not_found") || lower.contains("not found") {
        std::io::ErrorKind::NotFound
    } else {
        std::io::ErrorKind::Other
    };
    std::io::Error::new(kind, format!("{path} on the console: {msg}"))
}

fn helper_list(mgmt: &str, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>> {
    Ok(ps5upload_core::download::list_dir_all(mgmt, dir)
        .map_err(|e| helper_io(dir, e))?
        .into_iter()
        .map(|e| (e.name, e.kind == "dir", e.size))
        .collect())
}

impl RemoteFiles for HelperFiles {
    fn open(&self, path: &str) -> std::io::Result<Box<dyn ReadSeek>> {
        let (size, is_dir) = self.stat(path)?;
        if is_dir {
            return Err(std::io::Error::other(format!("{path} is a folder")));
        }
        Ok(Box::new(HelperReader {
            mgmt: self.mgmt.clone(),
            path: path.to_string(),
            size,
            pos: 0,
            buf: Vec::new(),
            buf_at: 0,
            window: HelperReader::CHUNK,
            conn: None,
        }))
    }

    fn stat(&self, path: &str) -> std::io::Result<(u64, bool)> {
        let p = path.trim_end_matches('/');
        let Some((parent, name)) = p.rsplit_once('/') else {
            return Ok((0, true));
        };
        if name.is_empty() {
            return Ok((0, true));
        }
        let parent = if parent.is_empty() { "/" } else { parent };
        self.cached_list(parent)?
            .into_iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, d, s)| (s, d))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{path} isn't on the console"),
                )
            })
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>> {
        let d = dir.trim_end_matches('/');
        self.cached_list(if d.is_empty() { "/" } else { d })
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

/// A console file read through the helper, over one connection kept open and the
/// pipelined FS_READ loop downloads use. The buffer starts at one chunk (header probes
/// stay cheap) and doubles while reads stay sequential, up to `MAX_WINDOW`.
struct HelperReader {
    mgmt: String,
    path: String,
    size: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_at: u64,
    window: u64,
    conn: Option<ps5upload_core::connection::Connection>,
}

impl HelperReader {
    const CHUNK: u64 = ps5upload_core::download::DOWNLOAD_CHUNK_SIZE;
    const MAX_WINDOW: u64 = 16 * HelperReader::CHUNK;

    fn fill(&mut self) -> std::io::Result<()> {
        // Sequential: this read starts where the buffer ended.
        if self.pos == self.buf_at + self.buf.len() as u64 && !self.buf.is_empty() {
            self.window = (self.window * 2).min(Self::MAX_WINDOW);
        } else {
            self.window = Self::CHUNK;
        }
        let end = (self.pos + self.window).min(self.size);
        let mut buf = Vec::with_capacity((end - self.pos) as usize);
        let mut at = self.pos;
        // A dropped connection mustn't fail a 50 GB conversion: reconnect and resume.
        let mut tries = 0u64;
        loop {
            let conn = match self.conn.as_mut() {
                Some(c) => c,
                None => match ps5upload_core::connection::Connection::connect(&self.mgmt) {
                    Ok(c) => self.conn.insert(c),
                    Err(e) => {
                        tries += 1;
                        if tries >= 4 {
                            return Err(helper_io(&self.path, e));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(250 * tries));
                        continue;
                    }
                },
            };
            match ps5upload_core::download::read_range_into(
                conn, &self.path, end, &mut buf, &mut at,
            ) {
                Ok(()) => break,
                Err(e) => {
                    self.conn = None;
                    tries += 1;
                    if tries >= 4 {
                        return Err(helper_io(&self.path, e));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250 * tries));
                }
            }
        }
        self.buf = buf;
        self.buf_at = self.pos;
        Ok(())
    }
}

impl std::io::Read for HelperReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.size || out.is_empty() {
            return Ok(0);
        }
        let in_buf = self.pos >= self.buf_at && self.pos < self.buf_at + self.buf.len() as u64;
        if !in_buf {
            self.fill()?;
        }
        let from = (self.pos - self.buf_at) as usize;
        let n = out.len().min(self.buf.len() - from);
        out[..n].copy_from_slice(&self.buf[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for HelperReader {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            std::io::SeekFrom::Start(n) => Some(n),
            std::io::SeekFrom::End(d) => self.size.checked_add_signed(d),
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before 0"))?;
        self.pos = next;
        Ok(next)
    }
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
    fn a_missing_console_path_reads_as_not_found() {
        let e = |m: &str| helper_io("/x", anyhow::anyhow!(m.to_string())).kind();
        use std::io::ErrorKind::{NotFound, Other};
        assert_eq!(
            e("payload rejected FS_LIST_DIR(/x): fs_list_dir_opendir_errno_2"),
            NotFound
        );
        assert_eq!(e("fs_list_dir_opendir_errno_20"), NotFound);
        // EACCES (13) and a dropped connection are not "missing".
        assert_eq!(e("fs_list_dir_opendir_errno_13"), Other);
        assert_eq!(e("read frame header: connection reset"), Other);
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
            console_target("ps5://10.0.0.2:9113/data/homebrew/G")
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

    /// Throughput of the console reader Convert uses, on a real file.
    /// Run by hand: PS5UPLOAD_SPEED_HOST=192.168.86.100 PS5UPLOAD_SPEED_FILE=/data/x.bin
    /// cargo test -p ps5upload-engine --release --lib helper_read_speed -- --ignored --nocapture
    /// Also checks the helper and a plain fs_read agree on a range away from the start.
    #[test]
    #[ignore]
    fn helper_read_speed() {
        use std::io::{Read, Seek};
        let host = std::env::var("PS5UPLOAD_SPEED_HOST").expect("PS5UPLOAD_SPEED_HOST");
        let path = std::env::var("PS5UPLOAD_SPEED_FILE").expect("PS5UPLOAD_SPEED_FILE");
        let limit: u64 = std::env::var("PS5UPLOAD_SPEED_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2 << 30);
        let files = HelperFiles::new(&host);
        let (size, is_dir) = files.stat(&path).unwrap();
        assert!(!is_dir && size > 0);
        let mut r = files.open(&path).unwrap();
        let probe = size / 3;
        r.seek(std::io::SeekFrom::Start(probe)).unwrap();
        let mut got = vec![0u8; 4096];
        r.read_exact(&mut got).unwrap();
        let want = ps5upload_core::fs_ops::fs_read(&files.mgmt, &path, probe, 4096).unwrap();
        assert_eq!(got, want);
        r.seek(std::io::SeekFrom::Start(0)).unwrap();
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
            "helper read: {:.2} GiB in {:.1} s = {:.1} MB/s",
            done as f64 / (1u64 << 30) as f64,
            secs,
            done as f64 / 1e6 / secs
        );
        let e = files.stat("/definitely/not/here.bin").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
    }
}
