//! SFTP servers through [`RemoteFs`], on the pure-Rust `russh` + `russh-sftp`.
//!
//! The host key is trusted on first use: the first connection is refused with the key's
//! fingerprint for the user to accept, and a different key later is refused as changed.
//! Paths are under the login folder, as with FTP.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::ftp_fs::under;
use super::host_key;
use super::store::{Connection, Secret};
use super::{Entry, Page, RemoteError, RemoteFile, RemoteFs, PAGE};

/// Longest wait for any one server step.
const STEP: Duration = Duration::from_secs(30);

async fn timed<T>(
    host: &str,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, RemoteError>>,
) -> Result<T, RemoteError> {
    tokio::time::timeout(STEP, fut).await.unwrap_or_else(|_| {
        Err(RemoteError::Io(format!(
            "{host}: no answer to {what} after 30 s"
        )))
    })
}

/// Accepts exactly the host key the user accepted, and records the one it was shown.
struct Tofu {
    accepted: Option<String>,
    seen: Arc<StdMutex<Option<String>>>,
}

impl client::Handler for Tofu {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let fp = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(HashAlg::Sha256).to_string()
            }
            PublicKeyOrCertificate::Certificate(c) => {
                c.public_key().fingerprint(HashAlg::Sha256).to_string()
            }
        };
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp.clone());
        Ok(self.accepted.as_deref() == Some(fp.as_str()))
    }
}

fn addr_of(conn: &Connection) -> String {
    let h = conn
        .host
        .trim()
        .trim_start_matches("sftp://")
        .trim_start_matches("ssh://");
    let h = h.split('/').next().unwrap_or(h);
    let h = h.rsplit_once('@').map(|(_, h)| h).unwrap_or(h);
    super::ftp_fs::host_port(h, conn.port)
}

fn sftp_err(host: &str, e: russh_sftp::client::error::Error) -> RemoteError {
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("no such file") || lower.contains("nosuchfile") {
        RemoteError::NotFound(msg)
    } else if lower.contains("permission") {
        RemoteError::Auth(msg)
    } else {
        RemoteError::Io(format!("{host}: {msg}"))
    }
}

pub struct SftpFs {
    host: String,
    /// What paths are under: the login folder, or "" for a connection from the server's `/`.
    home: String,
    login: String,
    sftp: Arc<SftpSession>,
    /// Keeps the SSH connection open for as long as the session is in use.
    _ssh: client::Handle<Tofu>,
}

impl SftpFs {
    pub async fn connect(conn: &Connection, secret: &Secret) -> Result<Self, RemoteError> {
        let addr = addr_of(conn);
        let seen = Arc::new(StdMutex::new(None));
        let handler = Tofu {
            accepted: conn.host_key.clone(),
            seen: Arc::clone(&seen),
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let connected = tokio::time::timeout(STEP, client::connect(config, addr.as_str(), handler))
            .await
            .map_err(|_| RemoteError::Unreachable(format!("{addr}: no answer after 30 s")))?;
        let mut ssh = match connected {
            Ok(h) => h,
            Err(e) => {
                let shown = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
                return Err(match shown {
                    Some(fp) if conn.host_key.as_deref() != Some(fp.as_str()) => {
                        host_key::check_host_key(conn.host_key.as_deref(), &fp)
                            .err()
                            .unwrap_or_else(|| RemoteError::Io(e.to_string()))
                    }
                    _ => RemoteError::Unreachable(format!("{addr}: {e}")),
                });
            }
        };
        let user = conn.user.clone();
        let auth = timed(&addr, "sign-in", async {
            let r = match secret {
                Secret::Key { pem, passphrase } => {
                    let key = russh::keys::decode_secret_key(pem, passphrase.as_deref()).map_err(
                        |e| RemoteError::Auth(format!("the key file could not be read: {e}")),
                    )?;
                    let hash = ssh
                        .best_supported_rsa_hash()
                        .await
                        .map_err(|e| RemoteError::Io(e.to_string()))?
                        .flatten();
                    ssh.authenticate_publickey(
                        user.clone(),
                        russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                    )
                    .await
                }
                Secret::Password { password } => {
                    ssh.authenticate_password(user.clone(), password.clone())
                        .await
                }
                Secret::None => ssh.authenticate_none(user.clone()).await,
            };
            r.map_err(|e| RemoteError::Io(e.to_string()))
        })
        .await?;
        if !auth.success() {
            return Err(RemoteError::Auth(format!(
                "{addr} refused the user or password"
            )));
        }
        let sftp = timed(&addr, "starting SFTP", async {
            let channel = ssh
                .channel_open_session()
                .await
                .map_err(|e| RemoteError::Io(e.to_string()))?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|e| RemoteError::Io(e.to_string()))?;
            SftpSession::new(channel.into_stream())
                .await
                .map_err(|e| RemoteError::Io(format!("the server has no SFTP: {e}")))
        })
        .await?;
        let login = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
        let home = if conn.from_root {
            String::new()
        } else {
            login.clone()
        };
        crate::log_info!("remote sftp: signed in to {addr}");
        Ok(Self {
            host: addr,
            home,
            login,
            sftp: Arc::new(sftp),
            _ssh: ssh,
        })
    }

    async fn entries(&self, path: &str) -> Result<Vec<Entry>, RemoteError> {
        let dir = under(&self.home, path);
        let host = self.host.as_str();
        let listed = timed(host, "a folder listing", async {
            self.sftp
                .read_dir(dir.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await?;
        let mut out: Vec<Entry> = listed
            .filter(|e| {
                let n = e.file_name();
                n != "." && n != ".."
            })
            .map(|e| {
                let m = e.metadata();
                let is_dir = m.is_dir();
                Entry {
                    name: e.file_name(),
                    is_dir,
                    size: if is_dir { 0 } else { m.size.unwrap_or(0) },
                    mtime: m.mtime.map(i64::from),
                }
            })
            .collect();
        out.sort_by_key(|e| e.name.to_lowercase());
        Ok(out)
    }
}

struct SftpFile {
    sftp: Arc<SftpSession>,
    host: String,
    path: String,
    size: u64,
}

#[async_trait::async_trait]
impl RemoteFile for SftpFile {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if offset >= self.size || len == 0 {
            return Ok(Vec::new());
        }
        let len = len.min(self.size - offset) as usize;
        let host = self.host.as_str();
        timed(host, "a read", async {
            let mut f = self
                .sftp
                .open(self.path.clone())
                .await
                .map_err(|e| sftp_err(host, e))?;
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| RemoteError::Io(format!("{host}: {e}")))?;
            let mut out = vec![0u8; len];
            let mut got = 0;
            while got < len {
                let n = f
                    .read(&mut out[got..])
                    .await
                    .map_err(|e| RemoteError::Io(format!("{host}: {e}")))?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            out.truncate(got);
            Ok(out)
        })
        .await
    }
}

#[async_trait::async_trait]
impl RemoteFs for SftpFs {
    fn login_dir(&self) -> Option<String> {
        Some(self.login.clone())
    }

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
        let full = under(&self.home, path);
        let host = self.host.as_str();
        let m = timed(host, "a file lookup", async {
            self.sftp
                .metadata(full.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await?;
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let is_dir = m.is_dir();
        Ok(Entry {
            name,
            is_dir,
            size: if is_dir { 0 } else { m.size.unwrap_or(0) },
            mtime: m.mtime.map(i64::from),
        })
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        let e = self.stat(path).await?;
        if e.is_dir {
            return Err(RemoteError::NotFound(format!("{path} is a folder")));
        }
        Ok(Arc::new(SftpFile {
            sftp: Arc::clone(&self.sftp),
            host: self.host.clone(),
            path: under(&self.home, path),
            size: e.size,
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
        let full = under(&self.home, path);
        let host = self.host.as_str();
        let made = timed(host, "making a folder", async {
            self.sftp
                .create_dir(full.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await;
        match made {
            Ok(()) => Ok(()),
            // SFTP v3 answers a taken name with a bare FAILURE: look to see what is there.
            Err(e) => match self.stat(path).await {
                Ok(st) if st.is_dir => Ok(()),
                Ok(_) => Err(RemoteError::Exists(format!("{path} is a file"))),
                Err(_) => Err(e),
            },
        }
    }

    async fn put_file(
        &self,
        path: &str,
        src: &std::path::Path,
        progress: &super::PutProgress,
    ) -> Result<(), RemoteError> {
        use tokio::io::AsyncWriteExt;
        let full = under(&self.home, path);
        let host = self.host.as_str();
        let mut out = self
            .sftp
            .create(full)
            .await
            .map_err(|e| sftp_err(host, e))?;
        let file = tokio::fs::File::open(src).await.map_err(super::local_io)?;
        let mut reader = super::AsyncCountingReader { file, progress };
        tokio::io::copy(&mut reader, &mut out)
            .await
            .map_err(|e| RemoteError::Io(format!("{host}: writing {path}: {e}")))?;
        out.shutdown()
            .await
            .map_err(|e| RemoteError::Io(format!("{host}: closing {path}: {e}")))?;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        let full = under(&self.home, path);
        let host = self.host.as_str();
        timed(host, "removing a file", async {
            self.sftp
                .remove_file(full.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await
    }

    /// SSH_FXP_RENAME (v3), which refuses an existing target (OpenSSH links the new name, so
    /// the refusal is atomic there). The posix-rename extension, which replaces, is never
    /// used. The target is also looked up first, for servers that bend the v3 rule.
    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        match self.stat(to).await {
            Ok(_) => return Err(RemoteError::Exists(format!("{to} already exists"))),
            Err(RemoteError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let (src, dst) = (under(&self.home, from), under(&self.home, to));
        let host = self.host.as_str();
        let done = timed(host, "a rename", async {
            self.sftp
                .rename(src.clone(), dst.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await;
        match done {
            // Something took the name meanwhile, and the file is still where it was.
            Err(e)
                if !matches!(e, RemoteError::NotFound(_))
                    && self.stat(to).await.is_ok()
                    && self.stat(from).await.is_ok() =>
            {
                Err(RemoteError::Exists(format!("{to} already exists ({e})")))
            }
            other => other,
        }
    }

    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        if path.trim_matches('/').is_empty() {
            return Err(RemoteError::BadPath(
                "the login folder cannot be removed".into(),
            ));
        }
        let full = under(&self.home, path);
        let host = self.host.as_str();
        timed(host, "removing a folder", async {
            self.sftp
                .remove_dir(full.clone())
                .await
                .map_err(|e| sftp_err(host, e))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docker run -d -p 2222:22 -v <dir>:/home/t/data atmoz/sftp t:t:1000`, then
    /// `PS5UPLOAD_SFTP_TEST=127.0.0.1:2222`. The first connection is refused with the host key's
    /// fingerprint; accepted, the server meets the contract; a different key is refused.
    #[tokio::test]
    #[ignore = "needs an SFTP server; see the doc comment"]
    async fn sftp_trusts_on_first_use_then_meets_the_contract() {
        let Ok(addr) = std::env::var("PS5UPLOAD_SFTP_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Sftp);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = "t".into();
        let secret = Secret::Password {
            password: "t".into(),
        };
        let fingerprint = match SftpFs::connect(&conn, &secret).await {
            Err(RemoteError::HostKey {
                fingerprint,
                changed: false,
            }) => fingerprint,
            Err(e) => panic!("expected an unknown key, got {e}"),
            Ok(_) => panic!("an unknown host key must not be trusted"),
        };
        assert!(fingerprint.starts_with("SHA256:"));
        conn.host_key = Some(fingerprint);
        let fs = SftpFs::connect(&conn, &secret).await.unwrap();
        crate::remote::contract::check(&fs, "/data").await;
        // A folder `data/w/` holding files `a` and `b` of different sizes, writable by t.
        if fs.stat("/data/w/a").await.is_ok() {
            crate::remote::contract::check_writes(&fs, "/data/w").await;
        }
        let wrong = Secret::Password {
            password: "nope".into(),
        };
        assert!(matches!(
            SftpFs::connect(&conn, &wrong).await,
            Err(RemoteError::Auth(_))
        ));
        conn.host_key = Some("SHA256:someone-else".into());
        assert!(matches!(
            SftpFs::connect(&conn, &secret).await,
            Err(RemoteError::HostKey { changed: true, .. })
        ));
    }

    /// A folder outside the user's home (Discord report, 2026-10-30: the start folder "No such
    /// file"). `docker run -d -p 2223:2222 -e USER_NAME=t -e USER_PASSWORD=t
    /// -e PASSWORD_ACCESS=true lscr.io/linuxserver/openssh-server` (home /config, not chrooted),
    /// with `/srv/games/sub/a.txt` ("hello\n") readable by t; then
    /// `PS5UPLOAD_SFTP_ROOT_TEST=127.0.0.1:2223`.
    #[tokio::test]
    #[ignore = "needs an SFTP server; see the doc comment"]
    async fn sftp_from_root_reaches_folders_outside_home() {
        let Ok(addr) = std::env::var("PS5UPLOAD_SFTP_ROOT_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Sftp);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = "t".into();
        let secret = Secret::Password {
            password: "t".into(),
        };
        if let Err(RemoteError::HostKey { fingerprint, .. }) = SftpFs::connect(&conn, &secret).await
        {
            conn.host_key = Some(fingerprint);
        }

        // Saved before 9.11: paths are under the login folder, so /srv is out of reach.
        let old = SftpFs::connect(&conn, &secret).await.unwrap();
        assert_eq!(old.login_dir().as_deref(), Some("/config"));
        assert!(matches!(
            old.list("/srv/games", None).await,
            Err(RemoteError::NotFound(_))
        ));

        conn.from_root = true;
        let fs = SftpFs::connect(&conn, &secret).await.unwrap();
        assert_eq!(fs.login_dir().as_deref(), Some("/config"));
        let names: Vec<String> = fs
            .list("/srv/games", None)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"sub".to_string()), "{names:?}");
        assert_eq!(fs.stat("/srv/games/sub/a.txt").await.unwrap().size, 6);
        let f = fs.open("/srv/games/sub/a.txt").await.unwrap();
        assert_eq!(f.read_at(0, 6).await.unwrap(), b"hello\n");
        // The login folder is still where it was, now by its own path.
        assert!(fs.list("/config", None).await.is_ok());
    }
}
