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

/// A file on the console: its path, its path relative to the root's parent, its size.
type ConsoleFile = (String, String, u64);

/// A source as the console lists it: its files, its folders, and what a relay leaves out
/// (links and other special entries, relative like the rest): a move keeps a source with any.
type Listing = (Vec<ConsoleFile>, Vec<String>, Vec<String>);

/// Every file under `root` on the console, and every folder (relative to `root`'s parent, the
/// root itself first among them): a folder with no file in it is made on the server too.
fn console_files(addr: &str, root: &str, kind: DownloadKind) -> Result<Listing> {
    let root = root.trim_end_matches('/');
    if matches!(kind, DownloadKind::File) {
        let base = root.rsplit('/').next().unwrap_or(root).to_string();
        let st = fs_ops::fs_stat(addr, root)?;
        return Ok((
            vec![(root.to_string(), base, st.size)],
            Vec::new(),
            Vec::new(),
        ));
    }
    walk_console(root, |dir, offset| {
        let l = fs_ops::list_dir(addr, dir, ListDirOptions { offset, limit: 256 })?;
        let entries = l
            .entries
            .into_iter()
            .map(|e| (e.name, e.kind, e.size))
            .collect();
        Ok((entries, l.truncated))
    })
}

/// The walk of [`console_files`], with the console's folder listing as `list(dir, offset)`:
/// one page of (name, kind, size) and whether more follow.
fn walk_console(
    root: &str,
    mut list: impl FnMut(&str, u64) -> Result<(Vec<(String, String, u64)>, bool)>,
) -> Result<Listing> {
    let base = root.rsplit('/').next().unwrap_or(root).to_string();
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped = Vec::new();
    let mut stack = vec![(root.to_string(), base)];
    while let Some((dir, rel)) = stack.pop() {
        dirs.push(rel.clone());
        let mut offset = 0u64;
        loop {
            let (entries, truncated) = list(&dir, offset)?;
            let n = entries.len() as u64;
            for (name, kind, size) in entries {
                if name == "." || name == ".." {
                    continue;
                }
                let path = format!("{dir}/{name}");
                // One name, as the server takes it: the listing comes from the console, and a
                // name holding `/` (or `..` behind a backslash, which a Windows or SMB server
                // reads as a separator) would be written outside the folder that was chosen.
                let r = remote::path::join(&rel, &name).map_err(remote_err)?;
                match kind.as_str() {
                    "dir" => stack.push((path, r)),
                    "file" => out.push((path, r, size)),
                    // A link, a device…: never copied, so a move must not delete it.
                    _ => skipped.push(r),
                }
            }
            if n == 0 || (n < 256 && !truncated) {
                break;
            }
            offset += n;
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    dirs.sort();
    skipped.sort();
    Ok((out, dirs, skipped))
}

fn remote_err(e: RemoteError) -> anyhow::Error {
    anyhow!("{}", remote::api::error_with_hint(&e))
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

/// The name a console file is staged under on this computer (what `to_local_escaped` writes).
fn staged_name(console_path: &str) -> Result<String> {
    let name = console_path.rsplit('/').next().unwrap_or(console_path);
    ps5upload_core::host_names::escape_name(name).map_err(|why| anyhow!(why))
}

/// The console end of a relay: what is copied, how one file comes off it, and (for a move) how
/// the source goes. A trait so the tests can stand a folder on this computer in for the console.
pub(crate) trait ConsoleSide {
    /// Every file and folder under the source, as [`console_files`] lists them.
    fn files(&self) -> Result<Listing>;
    /// Pulls the console file `path` into `staging`, under [`staged_name`], counting into `dl`.
    fn pull(
        &self,
        path: &str,
        staging: &Path,
        dl: &ps5upload_ava1::download::Counters,
        cancel: &Arc<AtomicBool>,
    ) -> Result<()>;
    /// Deletes the source (the file, or the whole folder tree) on the console.
    fn delete(&self) -> Result<()>;
}

/// The console itself, over AVA1.
struct Ava1Console<'a> {
    addr: &'a str,
    src: &'a str,
    kind: DownloadKind,
    unsafe_read: bool,
}

impl ConsoleSide for Ava1Console<'_> {
    fn files(&self) -> Result<Listing> {
        console_files(self.addr, self.src, self.kind)
    }

    fn pull(
        &self,
        path: &str,
        staging: &Path,
        dl: &ps5upload_ava1::download::Counters,
        cancel: &Arc<AtomicBool>,
    ) -> Result<()> {
        let id: [u8; 16] = ava1::keys::random_bytes()?;
        // Staged under a name this computer can hold whatever the console's is (`a:b`, `CON`,
        // `x.` on Windows): the server gets the console's own name, and it is the server, not
        // this computer's staging folder, that says whether it can hold it.
        ps5upload_ava1::download::to_local_escaped(
            self.addr,
            path,
            DownloadKind::File,
            staging,
            self.unsafe_read,
            id,
            dl,
            Some(Arc::clone(cancel)),
        )
        .map(|_| ())
    }

    fn delete(&self) -> Result<()> {
        // As the Files delete does: never from under a game that is running from it.
        if let Some(title) = crate::running_title_from(self.addr, self.src) {
            bail!(
                "{title} is running from {}. Close it first: {KEPT}.",
                self.src
            );
        }
        // A whole game folder takes minutes to delete (see the fs_delete route's deadline).
        fs_ops::fs_delete_with_op_id(self.addr, self.src, 0, Some(Duration::from_secs(60 * 60)))
    }
}

/// How a move that stopped short ends its message.
const KEPT: &str = "the original was kept on the PS5";

/// Whether `src` may be moved off the console: never a drive or a top folder (`/data`,
/// `/mnt/usb0`) as a whole, whose delete would take every other file on it along.
pub fn movable_source(src: &str) -> bool {
    let parts: Vec<&str> = src.split('/').filter(|p| !p.is_empty()).collect();
    if !src.starts_with('/') || parts.iter().any(|p| *p == "." || *p == "..") {
        return false;
    }
    match parts.first() {
        Some(&"mnt") => parts.len() > 2,
        Some(_) => parts.len() > 1,
        None => false,
    }
}

/// What a relay put on the server: every file (console path, path under `dest_dir`, size) and
/// every folder, as the console listed them.
pub(crate) struct Relayed {
    bytes: u64,
    dest_dir: String,
    files: Vec<ConsoleFile>,
    dirs: Vec<String>,
    skipped: Vec<String>,
}

/// Pulls `src` (a file or a folder) from the console at `addr` into `dest` (`remote://id/dir`).
/// With `remove_source` it is a move: the copy is checked on the server and the source deleted
/// only then ([`finish_move`]); a failed or cancelled copy leaves the source as it was.
/// Returns the bytes delivered.
#[allow(clippy::too_many_arguments)]
pub fn to_server(
    addr: &str,
    src: &str,
    kind: DownloadKind,
    dest: &str,
    unsafe_read: bool,
    remove_source: bool,
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
    let console = Ava1Console {
        addr,
        src,
        kind,
        unsafe_read,
    };
    copy_or_move(
        &rt,
        fs.as_ref(),
        &console,
        &rp.path,
        remove_source,
        counters,
        &cancel,
        staging_root,
    )
}

/// [`to_server`] past its setup: the relay, then for a move [`finish_move`]. The bytes delivered.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_or_move(
    rt: &tokio::runtime::Handle,
    fs: &dyn RemoteFs,
    console: &dyn ConsoleSide,
    dest_dir: &str,
    remove_source: bool,
    counters: &ps5upload_ava1::download::Counters,
    cancel: &Arc<AtomicBool>,
    staging_root: &Path,
) -> Result<u64> {
    let done = relay(rt, fs, console, dest_dir, counters, cancel, staging_root)?;
    if remove_source {
        finish_move(rt, fs, console, &done, cancel)?;
    }
    Ok(done.bytes)
}

/// The copy of [`to_server`]: every folder made, every file staged here and placed on the server.
pub(crate) fn relay(
    rt: &tokio::runtime::Handle,
    fs: &dyn RemoteFs,
    console: &dyn ConsoleSide,
    dest_dir: &str,
    counters: &ps5upload_ava1::download::Counters,
    cancel: &Arc<AtomicBool>,
    staging_root: &Path,
) -> Result<Relayed> {
    let (files, dirs, skipped) = console.files()?;
    let total: u64 = files.iter().map(|f| f.2).sum();
    if let Some(t) = &counters.total {
        t.store(total.max(1), Ordering::Relaxed);
    }
    let job: [u8; 8] = ava1::keys::random_bytes()?;
    let staging: PathBuf =
        staging_root.join(job.iter().map(|b| format!("{b:02x}")).collect::<String>());
    std::fs::create_dir_all(&staging).with_context(|| format!("make {}", staging.display()))?;
    let dest_dir = dest_dir.trim_end_matches('/').to_string();
    let result = (|| -> Result<u64> {
        let mut made = std::collections::HashSet::new();
        let mut done = 0u64;
        // Every folder, so one with nothing in it is on the server as it is on the console.
        for rel in &dirs {
            if cancel.load(Ordering::Relaxed) {
                bail!("transfer_cancelled");
            }
            let dir = format!("{dest_dir}/{rel}");
            rt.block_on(make_dirs(fs, &dir))?;
            made.insert(dir);
        }
        for (path, rel, size) in &files {
            if cancel.load(Ordering::Relaxed) {
                bail!("transfer_cancelled");
            }
            let target = format!("{dest_dir}/{rel}");
            if let Some((parent, _)) = target.rsplit_once('/') {
                if !parent.is_empty() && made.insert(parent.to_string()) {
                    rt.block_on(make_dirs(fs, parent))?;
                }
            }
            // 1. Off the console, into staging (half of this file's share of the bar).
            let dl = ps5upload_ava1::download::Counters::default();
            let dl_bytes = Arc::clone(&dl.bytes);
            let shown = Arc::clone(&counters.bytes);
            let base = done;
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
            let got = console.pull(path, &staging, &dl, cancel);
            stop.store(true, Ordering::Relaxed);
            let _ = mirror.join();
            got?;
            let staged = staging.join(staged_name(path)?);
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
                cancel: Arc::clone(cancel),
            };
            let placed = rt.block_on(place(fs, &staged, &target, &progress));
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
    Ok(Relayed {
        bytes: result?,
        dest_dir,
        files,
        dirs,
        skipped,
    })
}

/// A move's second half, once the copy finished: every file is on the server at its size and
/// every folder is there, the source on the console is still what was copied (nothing added,
/// grown or gone meanwhile), and only then is the source deleted. Any doubt keeps the source.
pub(crate) fn finish_move(
    rt: &tokio::runtime::Handle,
    fs: &dyn RemoteFs,
    console: &dyn ConsoleSide,
    r: &Relayed,
    cancel: &AtomicBool,
) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("transfer_cancelled");
    }
    if let Some(first) = r.skipped.first() {
        bail!("{first} is a link, which can't be copied to the server: {KEPT}.");
    }
    for (_, rel, size) in &r.files {
        let target = format!("{}/{rel}", r.dest_dir);
        match rt.block_on(fs.stat(&target)) {
            Ok(e) if !e.is_dir && e.size == *size => {}
            Ok(e) => bail!(
                "The copy of {rel} on the server is {} bytes, not {size}: {KEPT}.",
                e.size
            ),
            Err(e) => bail!(
                "The copy of {rel} could not be checked on the server ({}): {KEPT}.",
                remote::api::error_with_hint(&e)
            ),
        }
    }
    for rel in &r.dirs {
        let dir = format!("{}/{rel}", r.dest_dir);
        if !rt.block_on(fs.stat(&dir)).is_ok_and(|e| e.is_dir) {
            bail!("The folder {rel} is not on the server: {KEPT}.");
        }
    }
    let (files, dirs, skipped) = console.files()?;
    if files != r.files || dirs != r.dirs || skipped != r.skipped {
        bail!("Files on the PS5 changed during the copy: {KEPT}.");
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("transfer_cancelled");
    }
    console.delete()
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
        let dir = crate::test_tmp::temp_path(format!("relay-{}", std::process::id()));
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

    /// A folder with nothing in it is listed, so it is made on the server (it was dropped).
    #[test]
    fn a_folder_with_no_file_in_it_is_still_listed() {
        let (files, dirs, _) = walk_console("/data/game", |dir, offset| {
            let page: Vec<(String, String, u64)> = match (dir, offset) {
                ("/data/game", 0) => vec![
                    (".".into(), "dir".into(), 0),
                    ("saves".into(), "dir".into(), 0),
                    ("sub".into(), "dir".into(), 0),
                    ("eboot.bin".into(), "file".into(), 7),
                ],
                ("/data/game/sub", 0) => vec![("a.bin".into(), "file".into(), 3)],
                _ => Vec::new(),
            };
            Ok((page, false))
        })
        .unwrap();
        assert_eq!(
            files,
            vec![
                (
                    "/data/game/eboot.bin".to_string(),
                    "game/eboot.bin".to_string(),
                    7
                ),
                (
                    "/data/game/sub/a.bin".to_string(),
                    "game/sub/a.bin".to_string(),
                    3
                ),
            ]
        );
        assert_eq!(dirs, ["game", "game/saves", "game/sub"]);
    }

    /// A listing name that would leave the chosen folder fails the walk; an odd but plain one
    /// (a backslash inside a name, as some games have) is kept.
    #[test]
    fn a_listing_name_never_leaves_the_chosen_folder() {
        for bad in ["x/../../../../other/f", "..\\..\\f", "a\0b"] {
            let r = walk_console("/data/game", |dir, _| {
                let page = match dir {
                    "/data/game" => vec![(bad.to_string(), "file".to_string(), 1)],
                    _ => Vec::new(),
                };
                Ok((page, false))
            });
            assert!(r.is_err(), "{bad:?}: {r:?}");
        }
        let (files, _, _) = walk_console("/data/game", |dir, _| {
            let page = match dir {
                "/data/game" => {
                    vec![("options\\gabindings.xml".to_string(), "file".to_string(), 1)]
                }
                _ => Vec::new(),
            };
            Ok((page, false))
        })
        .unwrap();
        assert_eq!(files[0].1, "game/options\\gabindings.xml");
    }

    /// A console name Windows cannot hold is staged under another, and an ordinary one as is.
    #[test]
    fn a_file_is_staged_under_a_name_this_computer_can_hold() {
        assert_eq!(staged_name("/data/x/eboot.bin").unwrap(), "eboot.bin");
        let odd = staged_name("/data/x/a:b?.bin").unwrap();
        assert!(!odd.contains(':') && !odd.contains('?'), "{odd}");
        assert_eq!(
            ps5upload_core::host_names::unescape_name(&odd).unwrap(),
            "a:b?.bin"
        );
    }

    /// A folder on this computer standing in for the console: a pull copies a file into staging
    /// (failing, cancelling or coming up short on cue), and a delete removes the source.
    struct DirConsole {
        root: PathBuf,
        /// The pull with this number (1-based) fails as a dropped console.
        fail_at: Option<usize>,
        /// The pull with this number is cancelled, as Stop does mid-file.
        cancel_at: Option<usize>,
        /// Every pull stages one byte less than the file holds.
        short: bool,
        /// After the first pull, a file is added to the source (a game writing into it).
        grow: bool,
        pulls: std::sync::atomic::AtomicUsize,
        deletes: std::sync::atomic::AtomicUsize,
    }

    impl DirConsole {
        fn new(root: &Path) -> Self {
            Self {
                root: root.to_path_buf(),
                fail_at: None,
                cancel_at: None,
                short: false,
                grow: false,
                pulls: Default::default(),
                deletes: Default::default(),
            }
        }
    }

    impl ConsoleSide for DirConsole {
        fn files(&self) -> Result<Listing> {
            walk_console(self.root.to_str().unwrap(), |dir, offset| {
                if offset > 0 {
                    return Ok((Vec::new(), false));
                }
                let mut page = Vec::new();
                for e in std::fs::read_dir(dir)? {
                    let e = e?;
                    let md = std::fs::symlink_metadata(e.path())?;
                    let kind = if md.is_symlink() {
                        "link"
                    } else if md.is_dir() {
                        "dir"
                    } else {
                        "file"
                    };
                    page.push((
                        e.file_name().to_string_lossy().into_owned(),
                        kind.to_string(),
                        md.len(),
                    ));
                }
                Ok((page, false))
            })
        }

        fn pull(
            &self,
            path: &str,
            staging: &Path,
            dl: &ps5upload_ava1::download::Counters,
            cancel: &Arc<AtomicBool>,
        ) -> Result<()> {
            let n = self.pulls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_at == Some(n) {
                bail!("the console dropped the connection");
            }
            if self.cancel_at == Some(n) {
                cancel.store(true, Ordering::SeqCst);
                bail!("transfer_cancelled");
            }
            let mut bytes = std::fs::read(path)?;
            if self.short {
                bytes.pop();
            }
            dl.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            std::fs::write(staging.join(staged_name(path)?), bytes)?;
            if self.grow && n == 1 {
                std::fs::write(self.root.join("new.sav"), b"written meanwhile")?;
            }
            Ok(())
        }

        fn delete(&self) -> Result<()> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            std::fs::remove_dir_all(&self.root)?;
            Ok(())
        }
    }

    /// A temporary folder that goes when the test ends, passed or failed.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let n: [u8; 8] = ava1::keys::random_bytes().unwrap();
            let hex: String = n.iter().map(|b| format!("{b:02x}")).collect();
            let p = std::env::temp_dir().join(format!("relay-{tag}-{hex}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A game folder `game` with a file, a subfolder with a file, and an empty subfolder.
    fn game(at: &Path) -> PathBuf {
        let root = at.join("game");
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"eboot bytes").unwrap();
        std::fs::write(root.join("sce_sys/param.json"), b"{\"a\":1}").unwrap();
        root
    }

    /// Runs a copy or move of `console` into `/shared/PS5` on `fs`, from a thread outside the
    /// runtime as the job's blocking thread is.
    fn run(fs: &dyn RemoteFs, console: &DirConsole, remove: bool, staging: &Path) -> Result<u64> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let counters = ps5upload_ava1::download::Counters::default();
        let cancel = Arc::new(AtomicBool::new(false));
        copy_or_move(
            rt.handle(),
            fs,
            console,
            "/shared/PS5",
            remove,
            &counters,
            &cancel,
            staging,
        )
    }

    #[test]
    fn a_move_deletes_the_source_only_once_the_copy_is_on_the_server() {
        let tmp = Scratch::new("move");
        let console = DirConsole::new(&game(&tmp.0));
        let fs = MemFs::new(&[]);
        let staging = tmp.0.join("staging");
        assert_eq!(run(&fs, &console, true, &staging).unwrap(), 18);
        assert_eq!(
            fs.bytes("/shared/PS5/game/eboot.bin").unwrap(),
            b"eboot bytes"
        );
        assert_eq!(
            fs.bytes("/shared/PS5/game/sce_sys/param.json").unwrap(),
            b"{\"a\":1}"
        );
        assert!(futures_block(fs.stat("/shared/PS5/game/empty")).is_dir);
        assert_eq!(console.deletes.load(Ordering::SeqCst), 1);
        assert!(
            !console.root.exists(),
            "the source goes after a checked copy"
        );
        // Nothing left in staging on this computer.
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[test]
    fn a_copy_keeps_the_source() {
        let tmp = Scratch::new("copy");
        let console = DirConsole::new(&game(&tmp.0));
        let fs = MemFs::new(&[]);
        run(&fs, &console, false, &tmp.0.join("staging")).unwrap();
        assert_eq!(
            fs.bytes("/shared/PS5/game/eboot.bin").unwrap(),
            b"eboot bytes"
        );
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(console.root.join("eboot.bin").exists());
    }

    #[test]
    fn a_move_that_fails_part_way_keeps_the_source() {
        let tmp = Scratch::new("fail");
        let mut console = DirConsole::new(&game(&tmp.0));
        console.fail_at = Some(2);
        let fs = MemFs::new(&[]);
        let e = run(&fs, &console, true, &tmp.0.join("staging")).unwrap_err();
        assert!(e.to_string().contains("dropped"), "{e}");
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(console.root.join("eboot.bin").exists());
        assert!(console.root.join("sce_sys/param.json").exists());
    }

    #[test]
    fn a_cancelled_move_keeps_the_source() {
        let tmp = Scratch::new("cancel");
        let mut console = DirConsole::new(&game(&tmp.0));
        console.cancel_at = Some(2);
        let fs = MemFs::new(&[]);
        let e = run(&fs, &console, true, &tmp.0.join("staging")).unwrap_err();
        assert_eq!(e.to_string(), "transfer_cancelled");
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(console.root.join("eboot.bin").exists());
        assert!(fs.paths().iter().all(|p| !p.ends_with(".ps5upload-tmp")));
    }

    #[test]
    fn a_move_whose_copy_does_not_match_keeps_the_source() {
        let tmp = Scratch::new("short");
        let mut console = DirConsole::new(&game(&tmp.0));
        console.short = true;
        let fs = MemFs::new(&[]);
        let e = run(&fs, &console, true, &tmp.0.join("staging")).unwrap_err();
        assert!(e.to_string().contains("kept"), "{e}");
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(console.root.join("eboot.bin").exists());
    }

    #[test]
    fn a_move_whose_source_changed_meanwhile_keeps_it() {
        let tmp = Scratch::new("grow");
        let mut console = DirConsole::new(&game(&tmp.0));
        console.grow = true;
        let fs = MemFs::new(&[]);
        let e = run(&fs, &console, true, &tmp.0.join("staging")).unwrap_err();
        assert!(e.to_string().contains("changed"), "{e}");
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(console.root.join("new.sav").exists());
    }

    /// A link is never copied (the console lists it as a link, not a file), so a move with one
    /// in the tree keeps the whole source: deleting it would lose the link (#953).
    #[cfg(unix)]
    #[test]
    fn a_move_whose_tree_holds_a_link_keeps_the_source() {
        let tmp = Scratch::new("link");
        let root = game(&tmp.0);
        std::os::unix::fs::symlink("eboot.bin", root.join("sce_sys/alias")).unwrap();
        let console = DirConsole::new(&root);
        let (_, _, skipped) = console.files().unwrap();
        assert_eq!(skipped, ["game/sce_sys/alias"]);
        let fs = MemFs::new(&[]);
        let e = run(&fs, &console, true, &tmp.0.join("staging")).unwrap_err();
        assert!(
            e.to_string().contains("game/sce_sys/alias is a link"),
            "{e}"
        );
        assert!(e.to_string().contains("kept"), "{e}");
        assert_eq!(console.deletes.load(Ordering::SeqCst), 0);
        assert!(root.join("sce_sys/alias").symlink_metadata().is_ok());
        // The files themselves were copied; a copy (not a move) of the same tree succeeds.
        assert_eq!(
            fs.bytes("/shared/PS5/game/eboot.bin").unwrap(),
            b"eboot bytes"
        );
        run(&fs, &console, false, &tmp.0.join("staging")).unwrap();
    }

    #[test]
    fn a_single_file_moves_too() {
        let tmp = Scratch::new("file");
        let root = game(&tmp.0);
        struct OneFile(DirConsole, PathBuf);
        impl ConsoleSide for OneFile {
            fn files(&self) -> Result<Listing> {
                let len = std::fs::metadata(&self.1)?.len();
                Ok((
                    vec![(self.1.to_string_lossy().into(), "eboot.bin".into(), len)],
                    Vec::new(),
                    Vec::new(),
                ))
            }
            fn pull(
                &self,
                path: &str,
                staging: &Path,
                dl: &ps5upload_ava1::download::Counters,
                cancel: &Arc<AtomicBool>,
            ) -> Result<()> {
                self.0.pull(path, staging, dl, cancel)
            }
            fn delete(&self) -> Result<()> {
                Ok(std::fs::remove_file(&self.1)?)
            }
        }
        let one = OneFile(DirConsole::new(&root), root.join("eboot.bin"));
        let fs = MemFs::new(&[]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        copy_or_move(
            rt.handle(),
            &fs,
            &one,
            "/shared",
            true,
            &Default::default(),
            &cancel,
            &tmp.0.join("staging"),
        )
        .unwrap();
        assert_eq!(fs.bytes("/shared/eboot.bin").unwrap(), b"eboot bytes");
        assert!(!root.join("eboot.bin").exists());
        assert!(
            root.join("sce_sys/param.json").exists(),
            "only the file goes"
        );
    }

    #[test]
    fn only_items_inside_a_drive_can_be_moved() {
        for ok in [
            "/data/game",
            "/data/a/b",
            "/mnt/usb0/PS5",
            "/mnt/ext1/x/y",
            "/user/x",
        ] {
            assert!(movable_source(ok), "{ok}");
        }
        for bad in [
            "/",
            "",
            "/data",
            "/data/",
            "/mnt",
            "/mnt/usb0",
            "/mnt/usb0/",
            "data/x",
        ] {
            assert!(!movable_source(bad), "{bad}");
        }
        assert!(!movable_source("/data/../mnt/usb0"));
    }

    fn futures_block<T>(f: impl std::future::Future<Output = Result<T, RemoteError>>) -> T {
        tokio::runtime::Runtime::new().unwrap().block_on(f).unwrap()
    }

    /// The same move against real Samba (memory: smb-test-server recipe), with a writable share:
    /// `PS5UPLOAD_SAMBA_TEST=127.0.0.1:445 PS5UPLOAD_SAMBA_SHARE=games PS5UPLOAD_SAMBA_USER=tester
    /// PS5UPLOAD_SAMBA_PASS=pw123 cargo test -p ps5upload-engine -- --ignored moves_onto_samba`.
    #[test]
    #[ignore = "needs a Samba server; see the doc comment"]
    fn moves_onto_samba() {
        let Ok(addr) = std::env::var("PS5UPLOAD_SAMBA_TEST") else {
            return;
        };
        use crate::remote::store::Secret;
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Smb);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.share = std::env::var("PS5UPLOAD_SAMBA_SHARE").unwrap_or_default();
        conn.user = std::env::var("PS5UPLOAD_SAMBA_USER").unwrap_or_default();
        let secret = match std::env::var("PS5UPLOAD_SAMBA_PASS") {
            Ok(password) => Secret::Password { password },
            Err(_) => Secret::None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let fs = rt
            .block_on(crate::remote::smb_fs::SmbFs::connect(&conn, &secret))
            .unwrap();
        let tmp = Scratch::new("samba");
        let dest = format!("/relay-test-{}", std::process::id());
        let cancel = Arc::new(AtomicBool::new(false));
        let go = |console: &DirConsole| {
            copy_or_move(
                rt.handle(),
                &fs,
                console,
                &dest,
                true,
                &Default::default(),
                &cancel,
                &tmp.0.join("staging"),
            )
        };
        // A failed move first: nothing deleted.
        let mut failing = DirConsole::new(&game(&tmp.0));
        failing.fail_at = Some(2);
        assert!(go(&failing).is_err());
        assert!(failing.root.join("eboot.bin").exists());
        // Then the real one.
        let console = DirConsole::new(&failing.root);
        go(&console).unwrap();
        assert!(!console.root.exists());
        let st = rt
            .block_on(fs.stat(&format!("{dest}/game/sce_sys/param.json")))
            .unwrap();
        assert_eq!(st.size, 7);
        assert!(
            rt.block_on(fs.stat(&format!("{dest}/game/empty")))
                .unwrap()
                .is_dir
        );
        // Leave the share as it was.
        for p in [
            "game/eboot.bin",
            "game/sce_sys/param.json",
            "game/sce_sys",
            "game/empty",
            "game",
            "",
        ] {
            let path = format!("{dest}/{p}");
            let path = path.trim_end_matches('/');
            let _ = rt.block_on(fs.remove_file(path));
            let _ = rt.block_on(fs.remove_dir(path));
        }
    }

    #[tokio::test]
    async fn a_cancelled_write_leaves_nothing_behind() {
        let fs = MemFs::new(&[]);
        let dir = crate::test_tmp::temp_path(format!("relay-c-{}", std::process::id()));
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
