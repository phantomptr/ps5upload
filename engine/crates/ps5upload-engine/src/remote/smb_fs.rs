//! SMB2/3 shares through [`RemoteFs`], on the pure-Rust `smb2` crate.
//!
//! One signed-in session per connection. Metadata calls (list, stat) take the session in turn;
//! an opened file owns its own reader, whose positioned reads run concurrently — a streamed
//! install keeps several in flight, as `smb_range` always did.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use super::store::{Connection, Secret};
use super::{Entry, Page, RemoteError, RemoteFile, RemoteFs, PAGE};

type PendingCloses = Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<Result<(), smb2::Error>>>>>;

pub struct SmbFs {
    session: Mutex<(smb2::SmbClient, smb2::Tree)>,
    host: String,
    pending_closes: PendingCloses,
}

#[derive(Serialize)]
pub struct Share {
    pub name: String,
    pub comment: String,
}

/// `host:port` for the crate, from whatever the user typed as the host.
/// Whether `name` is a name Samba made up. A Samba server shows a file whose real name Windows
/// cannot hold (a `:`, `?`, `*`, a trailing dot or space…) under a hashed 8.3 name instead:
/// `colon:name.txt` lists as `C69YSY~1.TXT`, `x?.pkg` as `XTHJTC~A.PKG` (measured on Samba
/// 4.19). The real name cannot be recovered from it, so a game copied through it gets files
/// under the wrong names and does not start.
pub(crate) fn looks_renamed_by_samba(name: &str) -> bool {
    let ok = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_';
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let b = base.as_bytes();
    b.len() == 8
        && b[..6].iter().all(|&c| ok(c))
        && b[6] == b'~'
        && ok(b[7])
        && ext.len() <= 3
        && ext.bytes().all(ok)
        && !(name.contains('.') && ext.is_empty())
}

/// For a folder read from this computer's own disk: a NAS share mounted by the OS (a Windows
/// mapped drive, a share mounted in Finder) shows Samba's made-up names too, and nothing marks
/// it as SMB. The names among `rel_paths` (any path component) that look made up, and the
/// message that says so; None when there are none.
pub(crate) fn samba_renamed_message<'a>(
    rel_paths: impl Iterator<Item = &'a str>,
    folder: &str,
) -> Option<String> {
    let mut count = 0usize;
    let mut first: Option<&str> = None;
    for p in rel_paths {
        for part in p.split(['/', '\\']) {
            if looks_renamed_by_samba(part) {
                count += 1;
                first.get_or_insert(part);
            }
        }
    }
    let first = first?;
    Some(format!(
        "{count} file name(s) in {folder} were made up by a network share (for example \"{first}\"): \
         their real names hold characters Windows file sharing cannot, such as : ? * or a trailing \
         dot, and the real names cannot be read back through the share. Copied like this the game \
         would get the wrong file names and not start. Copy it from the server over SFTP or FTP \
         (add the server under Connections), or copy the game to this computer another way first."
    ))
}

fn server_addr(host: &str, port: u16) -> String {
    let mut h = host.trim();
    for scheme in ["smb://", "cifs://", "//", "\\\\"] {
        // `get`: a host named in Chinese has no letter boundary there (#663).
        if h.get(..scheme.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(scheme))
        {
            h = &h[scheme.len()..];
        }
    }
    let h = h.split(['/', '\\']).next().unwrap_or(h);
    if h.starts_with('[') {
        let end = h.find(']').map(|i| i + 1).unwrap_or(h.len());
        format!("{}:{port}", &h[..end])
    } else if h.matches(':').count() > 1 {
        format!("[{h}]:{port}")
    } else {
        format!("{}:{port}", h.split(':').next().unwrap_or(h))
    }
}

fn credentials(secret: &Secret) -> &str {
    match secret {
        Secret::Password { password } => password,
        _ => "",
    }
}

fn map_err(host: &str, e: smb2::Error) -> RemoteError {
    use smb2::ErrorKind as K;
    let msg = e.to_string();
    match e.kind() {
        K::AuthRequired | K::SigningRequired => RemoteError::Auth(msg),
        K::AccessDenied => RemoteError::Io(format!("SMB permission denied: {msg}")),
        K::NotFound | K::NotADirectory => RemoteError::NotFound(msg),
        K::AlreadyExists => RemoteError::Exists(msg),
        K::ConnectionLost | K::TimedOut => RemoteError::Unreachable(format!("{host}: {msg}")),
        _ => RemoteError::Io(msg),
    }
}

/// A tree connect that failed: no share by that name says so in words (the server's own
/// "STATUS_BAD_NETWORK_NAME during TreeConnect" means nothing to most people).
fn share_err(host: &str, share: &str, e: smb2::Error) -> RemoteError {
    if e.status() == Some(smb2::types::status::NtStatus::BAD_NETWORK_NAME) {
        return RemoteError::NotFound(no_share_message(share));
    }
    map_err(host, e)
}

fn no_share_message(share: &str) -> String {
    format!("No share named '{share}' on this server (STATUS_BAD_NETWORK_NAME).")
}

/// The share-relative form the crate takes: "/games/x" → "games/x", "/" → "".
fn share_path(p: &str) -> String {
    p.trim_matches('/').to_string()
}

fn mtime(t: smb2::pack::FileTime) -> Option<i64> {
    // 100 ns ticks since 1601; zero means unknown.
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    (t.0 > EPOCH_DIFF).then(|| ((t.0 - EPOCH_DIFF) / 10_000_000) as i64)
}

async fn sign_in(
    conn: &Connection,
    secret: &Secret,
) -> Result<(smb2::SmbClient, String), RemoteError> {
    let addr = server_addr(&conn.host, conn.port);
    let client = smb2::connect(&addr, &conn.user, credentials(secret))
        .await
        .map_err(|e| match e.kind() {
            smb2::ErrorKind::Io | smb2::ErrorKind::TimedOut | smb2::ErrorKind::ConnectionLost => {
                RemoteError::Unreachable(format!("{addr}: {e}"))
            }
            smb2::ErrorKind::AccessDenied => RemoteError::Auth(e.to_string()),
            _ => map_err(&addr, e),
        })?;
    Ok((client, addr))
}

pub async fn list_shares(conn: &Connection, secret: &Secret) -> Result<Vec<Share>, RemoteError> {
    let (mut client, addr) = sign_in(conn, secret).await?;
    let shares = client.list_shares().await.map_err(|e| map_err(&addr, e))?;
    Ok(shares
        .into_iter()
        .filter(|s| !s.name.ends_with('$'))
        .map(|s| Share {
            name: s.name,
            comment: s.comment,
        })
        .collect())
}

impl SmbFs {
    pub async fn connect(conn: &Connection, secret: &Secret) -> Result<Self, RemoteError> {
        let (mut client, addr) = sign_in(conn, secret).await?;
        let share = conn.share.trim_matches(['/', '\\']);
        let tree = client
            .connect_share(share)
            .await
            .map_err(|e| share_err(&addr, share, e))?;
        crate::log_info!("remote smb: signed in to {addr}");
        Ok(Self {
            session: Mutex::new((client, tree)),
            host: addr,
            pending_closes: Arc::default(),
        })
    }

    async fn finish_closes(&self) -> Result<(), RemoteError> {
        let pending = std::mem::take(
            &mut *self
                .pending_closes
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let mut error = None;
        for close in pending {
            match close.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    error.get_or_insert_with(|| map_err(&self.host, e));
                }
                Err(e) => {
                    error.get_or_insert_with(|| RemoteError::Io(format!("SMB close failed: {e}")));
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    async fn entries(&self, path: &str) -> Result<Vec<Entry>, RemoteError> {
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        let mut list: Vec<Entry> = client
            .list_directory(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))?
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| Entry {
                mtime: mtime(e.modified),
                name: e.name,
                is_dir: e.is_directory,
                size: e.size,
            })
            .collect();
        list.sort_by_key(|e| e.name.to_lowercase());
        Ok(list)
    }
}

struct SmbFile {
    reader: Option<smb2::FileReader>,
    runtime: tokio::runtime::Handle,
    pending_closes: PendingCloses,
    size: u64,
    host: String,
}

// smb2 closes a dropped FileReader on its own, but nothing can wait for that. Closing it here lets
// finish_closes wait, so a rename or delete of a game's folder doesn't meet a handle still open
// from a metadata read.
impl Drop for SmbFile {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            let close = self.runtime.spawn(async move { reader.close().await });
            self.pending_closes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(close);
        }
    }
}

#[async_trait::async_trait]
impl RemoteFile for SmbFile {
    fn size(&self) -> u64 {
        self.size
    }
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if offset >= self.size || len == 0 {
            return Ok(Vec::new());
        }
        let len = len.min(self.size - offset);
        self.reader
            .as_ref()
            .expect("live SMB reader")
            .read_at(offset, len)
            .await
            .map_err(|e| map_err(&self.host, e))
    }
}

/// A file being written on the share.
struct SmbWriter {
    writer: smb2::FileWriter,
    host: String,
}

#[async_trait::async_trait]
impl super::RemoteWriter for SmbWriter {
    async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError> {
        self.writer
            .write_chunk(data)
            .await
            .map_err(|e| map_err(&self.host, e))
    }

    async fn finish(self: Box<Self>) -> Result<(), RemoteError> {
        let host = self.host;
        self.writer
            .finish()
            .await
            .map(|_| ())
            .map_err(|e| map_err(&host, e))
    }

    async fn abort(self: Box<Self>) {
        let _ = self.writer.abort().await;
    }
}

#[async_trait::async_trait]
impl RemoteFs for SmbFs {
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError> {
        let all = self.entries(path).await?;
        let from = cursor
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0)
            .min(all.len());
        let to = (from + PAGE).min(all.len());
        Ok(Page {
            entries: all[from..to].to_vec(),
            next_cursor: (to < all.len()).then(|| to.to_string()),
        })
    }

    async fn stat(&self, path: &str) -> Result<Entry, RemoteError> {
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        if share_path(path).is_empty() {
            return Ok(Entry {
                name,
                is_dir: true,
                size: 0,
                mtime: None,
            });
        }
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        let info = client
            .stat(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))?;
        Ok(Entry {
            name,
            is_dir: info.is_directory,
            size: info.size,
            mtime: mtime(info.modified),
        })
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        // A file already read that wouldn't close says nothing about this one.
        if let Err(e) = self.finish_closes().await {
            crate::log_warn!(
                "remote smb: closing an earlier file on {} failed: {e}",
                self.host
            );
        }
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        let reader = client
            .open_file_reader(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))?;
        let size = reader.size();
        Ok(Arc::new(SmbFile {
            reader: Some(reader),
            runtime: tokio::runtime::Handle::current(),
            pending_closes: self.pending_closes.clone(),
            size,
            host: self.host.clone(),
        }))
    }

    async fn walk_until(
        &self,
        path: &str,
        limit: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<(String, Entry)>, RemoteError> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(super::walk_cancelled());
            }
            let dir = if rel.is_empty() {
                path.to_string()
            } else {
                format!("{}/{rel}", path.trim_end_matches('/'))
            };
            for e in self.entries(&dir).await? {
                let child = if rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{rel}/{}", e.name)
                };
                if e.is_dir {
                    stack.push(child);
                } else {
                    out.push((child, e));
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    async fn create_dir(&self, path: &str) -> Result<(), RemoteError> {
        let made = {
            let mut s = self.session.lock().await;
            let (client, tree) = &mut *s;
            client.create_directory(tree, &share_path(path)).await
        };
        match made.map_err(|e| map_err(&self.host, e)) {
            Err(RemoteError::Exists(m)) => match self.stat(path).await {
                Ok(e) if e.is_dir => Ok(()),
                _ => Err(RemoteError::Exists(m)),
            },
            other => other,
        }
    }

    /// SMB2 SET_INFO FileRenameInformation with ReplaceIfExists = false (the crate's only
    /// form): the server itself refuses a taken name with OBJECT_NAME_COLLISION.
    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        // Finish closes started by inspection before asking the server to move its folder.
        self.finish_closes().await?;
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        client
            .rename(tree, &share_path(from), &share_path(to))
            .await
            .map_err(|e| map_err(&self.host, e))
    }

    /// The crate's pipelined writer, which owns its own handle on the connection: the session
    /// stays free for listings while a file is written. From a byte past the start, the file
    /// is opened without truncating and written on from there.
    async fn create_writer<'a>(
        &'a self,
        path: &str,
        _size: u64,
        from: u64,
    ) -> Result<Box<dyn super::RemoteWriter + 'a>, RemoteError> {
        self.finish_closes().await?;
        let writer = {
            let mut s = self.session.lock().await;
            let (client, tree) = &mut *s;
            let opened = if from == 0 {
                client.create_file_writer(tree, &share_path(path)).await
            } else {
                client
                    .create_file_writer_at(tree, &share_path(path), from)
                    .await
            };
            opened.map_err(|e| map_err(&self.host, e))?
        };
        Ok(Box::new(SmbWriter {
            writer,
            host: self.host.clone(),
        }))
    }

    fn writes_from(&self) -> bool {
        true
    }

    /// The share's room for this user (FileFsFullSizeInformation's caller-available units).
    async fn free_space(&self, _path: &str) -> Result<Option<u64>, RemoteError> {
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        let info = client
            .fs_info(tree)
            .await
            .map_err(|e| map_err(&self.host, e))?;
        Ok(Some(info.free_bytes))
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        self.finish_closes().await?;
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        client
            .delete_file(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))
    }

    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        if share_path(path).is_empty() {
            return Err(RemoteError::BadPath(
                "the share itself cannot be removed".into(),
            ));
        }
        self.finish_closes().await?;
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        client
            .delete_directory(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_mounted_share_with_made_up_names_is_caught_and_a_normal_game_is_not() {
        let game = ["sce_sys/param.json", "eboot.bin", "Media/Data/level1.pak"];
        assert!(samba_renamed_message(game.into_iter(), "Z:\\Games\\G").is_none());
        let renamed = ["S0YQZ1~A/param.json", "eboot.bin", "Media/C69YSY~1.TXT"];
        let msg = samba_renamed_message(renamed.into_iter(), "Z:\\Games\\G").unwrap();
        assert!(msg.starts_with("2 file name(s) in Z:\\Games\\G"), "{msg}");
        assert!(msg.contains("S0YQZ1~A"), "{msg}");
    }

    #[test]
    fn samba_made_up_names_are_recognised() {
        // Every one listed by Samba 4.19 for a name Windows cannot hold.
        for made_up in [
            "C69YSY~1.TXT",
            "Q822F5~G.TXT",
            "TYCOL1~X",
            "_PTVME~B.TXT",
            "U8JPQ9~I.ELF",
            "XTHJTC~A.PKG",
            "LPM47I~9",
        ] {
            assert!(super::looks_renamed_by_samba(made_up), "{made_up}");
        }
        for real in [
            "eboot.bin",
            "param.json",
            "Case.txt",
            "a very long file name.bin",
            "ABCDEFG~1.TXT",
            "abcdef~1.txt",
            "ABCDEF~1.",
            "ABCDEF~1.TEXT",
            "ABCDEF_1.TXT",
        ] {
            assert!(!super::looks_renamed_by_samba(real), "{real}");
        }
    }

    use super::*;

    #[test]
    fn a_denied_rename_is_not_a_failed_sign_in() {
        let error = smb2::Error::Protocol {
            status: smb2::types::status::NtStatus::ACCESS_DENIED,
            command: smb2::types::Command::SetInfo,
        };
        let mapped = map_err("nas", error);
        assert!(!matches!(mapped, RemoteError::Auth(_)));
        assert!(mapped.to_string().contains("permission"));
    }

    #[test]
    fn a_server_named_in_chinese_gets_its_port() {
        assert_eq!(server_addr("群晖NAS", 445), "群晖NAS:445");
        assert_eq!(server_addr("smb://群晖", 445), "群晖:445");
    }

    #[test]
    fn a_missing_share_is_named_in_words() {
        let error = smb2::Error::Protocol {
            status: smb2::types::status::NtStatus::BAD_NETWORK_NAME,
            command: smb2::types::Command::TreeConnect,
        };
        let msg = share_err("nas:445", "XXX", error).to_string();
        assert!(
            msg.starts_with("No share named 'XXX' on this server"),
            "{msg}"
        );
        // The hint still finds it, and explains where the folders go.
        assert!(crate::remote::hints::hint_for(&msg)
            .unwrap()
            .contains("first folder name only"));
        let other = smb2::Error::Protocol {
            status: smb2::types::status::NtStatus::ACCESS_DENIED,
            command: smb2::types::Command::TreeConnect,
        };
        assert!(share_err("nas:445", "XXX", other)
            .to_string()
            .contains("permission"));
    }

    #[test]
    fn server_addresses_from_what_people_type() {
        assert_eq!(server_addr("nas", 445), "nas:445");
        assert_eq!(server_addr("smb://nas/games", 445), "nas:445");
        assert_eq!(server_addr("\\\\PC\\PS5PKG", 445), "PC:445");
        assert_eq!(server_addr("10.0.0.5:4450", 445), "10.0.0.5:445");
        assert_eq!(server_addr("fe80::1", 445), "[fe80::1]:445");
        assert_eq!(server_addr("[fe80::1]", 1445), "[fe80::1]:1445");
        assert_eq!(share_path("/games/ps5/"), "games/ps5");
        assert_eq!(share_path("/"), "");
    }

    /// `docker run -d -p 1445:445 dperson/samba -s "games;/share;yes;no;yes"` with a file or two
    /// in /share, then `PS5UPLOAD_SAMBA_TEST=127.0.0.1:1445 cargo test -- --ignored smb_meets`.
    #[tokio::test]
    #[ignore = "needs a Samba server; see the doc comment"]
    async fn smb_meets_the_remote_contract() {
        let Ok(addr) = std::env::var("PS5UPLOAD_SAMBA_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Smb);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = std::env::var("PS5UPLOAD_SAMBA_USER").unwrap_or_default();
        let secret = match std::env::var("PS5UPLOAD_SAMBA_PASS") {
            Ok(password) => Secret::Password { password },
            Err(_) => Secret::None,
        };
        let fs = SmbFs::connect(&conn, &secret).await.unwrap();
        crate::remote::contract::check(&fs, "/").await;
        // A writable share with files `w/a` and `w/b` of different sizes. The share's folder must
        // keep extended attributes: on a Docker Desktop bind mount Samba answers every rename
        // with STATUS_NOT_SUPPORTED, so seed the files inside the container.
        if fs.stat("/w/a").await.is_ok() {
            crate::remote::contract::check_writes(&fs, "/w").await;
            // An extracted game is a populated directory, not an empty folder.
            if fs.stat("/w/game/sce_sys/param.json").await.is_ok() {
                let size = fs.stat("/w/game/sce_sys/param.json").await.unwrap().size;
                // Collection inspection opens metadata before it tries to move the folder.
                // Dropping that reader must close the SMB handle, not leave it in the session.
                let metadata = fs.open("/w/game/sce_sys/param.json").await.unwrap();
                assert!(!metadata.read_at(0, 1024).await.unwrap().is_empty());
                drop(metadata);
                fs.rename("/w/game", "/w/PS5/game")
                    .await
                    .expect("move an intact game directory");
                assert_eq!(
                    fs.stat("/w/PS5/game/sce_sys/param.json")
                        .await
                        .unwrap()
                        .size,
                    size
                );
                assert!(matches!(
                    fs.stat("/w/game").await,
                    Err(RemoteError::NotFound(_))
                ));
                fs.rename("/w/PS5/game", "/w/game")
                    .await
                    .expect("undo intact game move");
            }
        }
    }
}
