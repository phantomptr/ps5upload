//! Downloads from a console straight to a saved server (8.0.0): a NAS share, FTP, SFTP or WebDAV,
//! for when the file should end up on the NAS rather than on this computer (the Docker engine
//! running on that NAS, say).
//!
//! The console only speaks AVA1, so the bytes pass through this engine: each file is pulled into a
//! staging folder under the data folder, written to the server under a temporary name, put in
//! place, and the staged copy deleted before the next file. The engine therefore needs room for
//! the largest single file, not the whole download.
//!
//! Progress counts each file half when it has come off the console and half as it reaches the
//! server, so the bar moves through both halves.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ps5upload_core::download::DownloadKind;
use ps5upload_core::fs_ops::{self, ListDirOptions};

use crate::remote::{self, PutProgress, RemoteError, RemoteFs};

/// Whether `dest` names a saved server rather than a folder on this computer.
pub fn is_server_dest(dest: &str) -> bool {
    remote::path::is_remote(dest)
}

/// Every file under `root` on the console, as (path relative to `root`'s parent, size).
fn console_files(addr: &str, root: &str, kind: DownloadKind) -> Result<Vec<(String, String, u64)>> {
    let root = root.trim_end_matches('/');
    let base = root.rsplit('/').next().unwrap_or(root).to_string();
    if matches!(kind, DownloadKind::File) {
        let st = fs_ops::fs_stat(addr, root)?;
        return Ok(vec![(root.to_string(), base, st.size)]);
    }
    let mut out = Vec::new();
    let mut stack = vec![(root.to_string(), base)];
    while let Some((dir, rel)) = stack.pop() {
        let mut offset = 0u64;
        loop {
            let l = fs_ops::list_dir(addr, &dir, ListDirOptions { offset, limit: 256 })?;
            let n = l.entries.len() as u64;
            for e in l.entries {
                if e.name == "." || e.name == ".." {
                    continue;
                }
                let path = format!("{dir}/{}", e.name);
                let r = format!("{rel}/{}", e.name);
                match e.kind.as_str() {
                    "dir" => stack.push((path, r)),
                    "file" => out.push((path, r, e.size)),
                    _ => {}
                }
            }
            if n == 0 || (n < 256 && !l.truncated) {
                break;
            }
            offset += n;
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

fn remote_err(e: RemoteError) -> anyhow::Error {
    anyhow!("{e}")
}

/// Makes `dir` and every missing parent on the server.
pub(crate) async fn make_dirs(fs: &dyn RemoteFs, dir: &str) -> Result<()> {
    let mut at = String::new();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        at = format!("{at}/{part}");
        fs.create_dir(&at).await.map_err(remote_err)?;
    }
    Ok(())
}

/// Writes `staged` to `target` on the server: under a temporary name first, then into place, so
/// an interrupted write never leaves a half file under the real name.
pub(crate) async fn place(
    fs: &dyn RemoteFs,
    staged: &Path,
    target: &str,
    progress: &PutProgress,
) -> Result<()> {
    let tmp = format!("{target}.ps5upload-tmp");
    if let Err(e) = fs.put_file(&tmp, staged, progress).await {
        let _ = fs.remove_file(&tmp).await;
        if progress.cancelled() {
            bail!("transfer_cancelled");
        }
        return Err(remote_err(e));
    }
    // A file already there is replaced, as a download into a folder replaces. Not every server
    // renames over a file, so the old one steps aside first and comes back if the swap fails:
    // a failure never leaves the server with neither.
    let old = format!("{target}.ps5upload-old");
    let had_old = match fs.stat(target).await {
        Ok(e) if !e.is_dir => {
            let _ = fs.remove_file(&old).await;
            fs.rename(target, &old).await.map_err(remote_err)?;
            true
        }
        Ok(_) => bail!("{target} is a folder on the server"),
        Err(_) => false,
    };
    match fs.rename(&tmp, target).await {
        Ok(()) => {
            if had_old {
                let _ = fs.remove_file(&old).await;
            }
            Ok(())
        }
        Err(e) => {
            if had_old {
                let _ = fs.rename(&old, target).await;
            }
            let _ = fs.remove_file(&tmp).await;
            Err(remote_err(e))
        }
    }
}

/// Pulls `src` (a file or a folder) from the console at `addr` into `dest` (`remote://id/dir`).
/// Returns the bytes delivered.
#[allow(clippy::too_many_arguments)]
pub fn to_server(
    addr: &str,
    src: &str,
    kind: DownloadKind,
    dest: &str,
    unsafe_read: bool,
    counters: &ps5upload_ava1::download::Counters,
    cancel: Arc<AtomicBool>,
    staging_root: &Path,
) -> Result<u64> {
    let rt = tokio::runtime::Handle::current();
    let rp = remote::path::parse(dest).map_err(remote_err)?;
    let r = remote::pool::global().map_err(remote_err)?;
    let fs = rt
        .block_on(r.pool.fs(&r.store, &rp.connection_id))
        .map_err(remote_err)?;
    let files = console_files(addr, src, kind)?;
    let total: u64 = files.iter().map(|f| f.2).sum();
    if let Some(t) = &counters.total {
        t.store(total.max(1), Ordering::Relaxed);
    }
    let job: [u8; 8] = ava1::keys::random_bytes()?;
    let staging: PathBuf =
        staging_root.join(job.iter().map(|b| format!("{b:02x}")).collect::<String>());
    std::fs::create_dir_all(&staging).with_context(|| format!("make {}", staging.display()))?;
    let result = (|| -> Result<u64> {
        let dest_dir = rp.path.trim_end_matches('/').to_string();
        let mut made = std::collections::HashSet::new();
        let mut done = 0u64;
        for (path, rel, size) in &files {
            if cancel.load(Ordering::Relaxed) {
                bail!("transfer_cancelled");
            }
            let target = format!("{dest_dir}/{rel}");
            if let Some((parent, _)) = target.rsplit_once('/') {
                if !parent.is_empty() && made.insert(parent.to_string()) {
                    rt.block_on(make_dirs(fs.as_ref(), parent))?;
                }
            }
            // 1. Off the console, into staging (half of this file's share of the bar).
            let dl = ps5upload_ava1::download::Counters::default();
            let dl_bytes = Arc::clone(&dl.bytes);
            let shown = Arc::clone(&counters.bytes);
            let base = done;
            // Before the progress thread starts: an early return must not leave it running.
            let id: [u8; 16] = ava1::keys::random_bytes()?;
            let stop = Arc::new(AtomicBool::new(false));
            let mirror = {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        shown.store(
                            base + dl_bytes.load(Ordering::Relaxed) / 2,
                            Ordering::Relaxed,
                        );
                        std::thread::sleep(Duration::from_millis(200));
                    }
                })
            };
            let got = ps5upload_ava1::download::to_local(
                addr,
                path,
                DownloadKind::File,
                &staging,
                unsafe_read,
                id,
                &dl,
                Some(Arc::clone(&cancel)),
            );
            stop.store(true, Ordering::Relaxed);
            let _ = mirror.join();
            got?;
            let name = path.rsplit('/').next().unwrap_or(path);
            let staged = staging.join(name);
            // 2. Onto the server (the other half).
            let half = size / 2;
            counters.bytes.store(base + half, Ordering::Relaxed);
            let sent = Arc::new(AtomicU64::new(0));
            let progress = PutProgress {
                add: {
                    let sent = Arc::clone(&sent);
                    let shown = Arc::clone(&counters.bytes);
                    let size = *size;
                    Box::new(move |n| {
                        let s = (sent.fetch_add(n, Ordering::Relaxed) + n).min(size);
                        // The second half of this file's share, in proportion to what was sent.
                        let part = if size == 0 {
                            0
                        } else {
                            (u128::from(s) * u128::from(size - half) / u128::from(size)) as u64
                        };
                        shown.store(base + half + part, Ordering::Relaxed);
                    })
                },
                cancel: Arc::clone(&cancel),
            };
            let placed = rt.block_on(place(fs.as_ref(), &staged, &target, &progress));
            let _ = std::fs::remove_file(&staged);
            placed?;
            done += size;
            counters.bytes.store(done, Ordering::Relaxed);
            counters.bytes_finalized.store(done, Ordering::Relaxed);
            counters.files.fetch_add(1, Ordering::Relaxed);
            counters.files_finalized.fetch_add(1, Ordering::Relaxed);
        }
        Ok(done)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::MemFs;

    #[test]
    fn server_destinations_are_told_apart_from_local_folders() {
        assert!(is_server_dest("remote://abc123/games"));
        assert!(!is_server_dest("/Users/me/Downloads"));
        assert!(!is_server_dest("C:\\Downloads"));
    }

    #[tokio::test]
    async fn a_file_lands_under_its_name_through_a_temporary_one() {
        let fs = MemFs::new(&[("/games/old.bin", b"stale")]);
        let dir = std::env::temp_dir().join(format!("relay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("old.bin");
        std::fs::write(&staged, b"fresh bytes").unwrap();
        let seen = Arc::new(AtomicU64::new(0));
        let s2 = Arc::clone(&seen);
        let progress = PutProgress {
            add: Box::new(move |n| {
                s2.fetch_add(n, Ordering::Relaxed);
            }),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        make_dirs(&fs, "/games/sub").await.unwrap();
        place(&fs, &staged, "/games/old.bin", &progress)
            .await
            .unwrap();
        assert_eq!(fs.bytes("/games/old.bin").unwrap(), b"fresh bytes");
        assert!(fs.bytes("/games/old.bin.ps5upload-tmp").is_none());
        assert_eq!(seen.load(Ordering::Relaxed), 11);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_cancelled_write_leaves_nothing_behind() {
        let fs = MemFs::new(&[]);
        let dir = std::env::temp_dir().join(format!("relay-c-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("x.bin");
        std::fs::write(&staged, vec![7u8; 3 << 20]).unwrap();
        let progress = PutProgress {
            add: Box::new(|_| {}),
            cancel: Arc::new(AtomicBool::new(true)),
        };
        let e = place(&fs, &staged, "/x.bin", &progress).await.unwrap_err();
        assert_eq!(e.to_string(), "transfer_cancelled");
        assert!(fs.bytes("/x.bin").is_none() && fs.bytes("/x.bin.ps5upload-tmp").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
