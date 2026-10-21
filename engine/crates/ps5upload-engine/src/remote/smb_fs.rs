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

pub struct SmbFs {
    session: Mutex<(smb2::SmbClient, smb2::Tree)>,
    host: String,
}

#[derive(Serialize)]
pub struct Share {
    pub name: String,
    pub comment: String,
}

/// `host:port` for the crate, from whatever the user typed as the host.
fn server_addr(host: &str, port: u16) -> String {
    let mut h = host.trim();
    for scheme in ["smb://", "cifs://", "//", "\\\\"] {
        if h.len() >= scheme.len() && h[..scheme.len()].eq_ignore_ascii_case(scheme) {
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
        let tree = client
            .connect_share(conn.share.trim_matches(['/', '\\']))
            .await
            .map_err(|e| map_err(&addr, e))?;
        crate::log_info!("remote smb: signed in to {addr}");
        Ok(Self {
            session: Mutex::new((client, tree)),
            host: addr,
        })
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
    reader: Arc<smb2::FileReader>,
    size: u64,
    host: String,
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
            .read_at(offset, len)
            .await
            .map_err(|e| map_err(&self.host, e))
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
        let s = self.session.lock().await;
        let (client, tree) = &*s;
        let reader = client
            .open_file_reader(tree, &share_path(path))
            .await
            .map_err(|e| map_err(&self.host, e))?;
        let size = reader.size();
        Ok(Arc::new(SmbFile {
            reader: Arc::new(reader),
            size,
            host: self.host.clone(),
        }))
    }

    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
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
        let mut s = self.session.lock().await;
        let (client, tree) = &mut *s;
        client
            .rename(tree, &share_path(from), &share_path(to))
            .await
            .map_err(|e| map_err(&self.host, e))
    }

    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        if share_path(path).is_empty() {
            return Err(RemoteError::BadPath(
                "the share itself cannot be removed".into(),
            ));
        }
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
