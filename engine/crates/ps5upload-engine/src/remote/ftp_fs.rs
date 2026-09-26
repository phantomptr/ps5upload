//! FTP and FTPS (explicit TLS) servers through [`RemoteFs`], on `suppaftp`.
//!
//! One control session serves listing; an opened file keeps a few sessions of its own, since
//! FTP reads one transfer at a time per session and a streamed install asks for several ranges
//! at once. A positioned read is `REST <offset>` + `RETR`, cut short with `ABOR` once enough
//! bytes have arrived.
//!
//! FTPS certificates are trusted on first use, like SFTP host keys: most home NAS boxes use a
//! self-signed certificate, which no certificate authority would vouch for.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::UNIX_EPOCH;

use suppaftp::list::ListParser;
use suppaftp::tokio::AsyncRustlsFtpStream as Ftp;
use suppaftp::types::FileType;
use suppaftp::FtpError;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use super::host_key;
use super::store::{Connection, Secret};
use super::{Entry, Page, RemoteError, RemoteFile, RemoteFs, PAGE};

/// Most sessions one open file keeps for concurrent reads.
const READ_SESSIONS: usize = 4;
/// Longest wait for any one server step (connect, sign-in, a listing, a read). A server that
/// goes quiet must fail the job, not freeze it.
const STEP: std::time::Duration = std::time::Duration::from_secs(30);

/// `fut`, or an error once the server has been silent for [`STEP`].
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

/// Turn a directory listing into entries: MLSD lines when the server speaks it, else the
/// Unix `ls -l` or Windows `DIR` lines `LIST` returns. `.` and `..` are dropped.
pub fn parse_listing(lines: &[String], mlsd: bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = lines
        .iter()
        .filter_map(|l| {
            let f = if mlsd {
                ListParser::parse_mlsd(l).ok()?
            } else {
                ListParser::parse_posix(l)
                    .or_else(|_| ListParser::parse_dos(l))
                    .ok()?
            };
            let name = f.name().to_string();
            if name.is_empty() || name == "." || name == ".." {
                return None;
            }
            let is_dir = f.is_directory();
            Some(Entry {
                name,
                is_dir,
                size: if is_dir { 0 } else { f.size() as u64 },
                mtime: f
                    .modified()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as i64)
                    .filter(|t| *t > 0),
            })
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

#[derive(Clone)]
struct Target {
    conn: Connection,
    secret: Secret,
    tls: bool,
}

fn addr_of(conn: &Connection) -> String {
    let h = conn
        .host
        .trim()
        .trim_start_matches("ftp://")
        .trim_start_matches("ftps://");
    let h = h.split('/').next().unwrap_or(h);
    if h.matches(':').count() > 1 && !h.starts_with('[') {
        format!("[{h}]:{}", conn.port)
    } else {
        format!("{}:{}", h.split(':').next().unwrap_or(h), conn.port)
    }
}

fn map_err(host: &str, e: FtpError) -> RemoteError {
    match &e {
        FtpError::ConnectionError(_) => RemoteError::Unreachable(format!("{host}: {e}")),
        FtpError::UnexpectedResponse(r) => {
            let code = r.status.code();
            let body = String::from_utf8_lossy(&r.body).trim().to_string();
            match code {
                530 | 331 | 332 => RemoteError::Auth(body),
                550 => RemoteError::NotFound(body),
                421 | 425 | 426 => RemoteError::Io(body),
                _ => RemoteError::Io(body),
            }
        }
        _ => RemoteError::Io(e.to_string()),
    }
}

/// A certificate verifier that accepts exactly the fingerprint the user accepted, and records
/// the one it was shown so an unknown or changed certificate can be offered for acceptance.
#[derive(Debug)]
struct Tofu {
    accepted: Option<String>,
    seen: Arc<StdMutex<Option<String>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Tofu {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = host_key::fingerprint(end_entity.as_ref());
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp.clone());
        if self.accepted.as_deref() == Some(fp.as_str()) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "server certificate not accepted".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

async fn sign_in(t: &Target) -> Result<Ftp, RemoteError> {
    let addr = addr_of(&t.conn);
    let mut ftp = timed(&addr, "connecting", async {
        Ftp::connect(addr.as_str())
            .await
            .map_err(|e| RemoteError::Unreachable(format!("{addr}: {e}")))
    })
    .await?;
    // A NAS behind a router often names its private address for passive data; use the one
    // we connected to instead.
    ftp.set_passive_nat_workaround(true);
    if t.tls {
        let seen = Arc::new(StdMutex::new(None));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        // TLS 1.2: vsftpd (behind many NAS FTP servers) requires each data connection to resume
        // the control connection's TLS session, and that check fails under TLS 1.3 — curl
        // cannot list such a server either. Resumption under 1.2 satisfies it.
        let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS12])
            .map_err(|e| RemoteError::Io(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Tofu {
                accepted: t.conn.host_key.clone(),
                seen: Arc::clone(&seen),
                provider,
            }))
            .with_no_client_auth();
        let connector = suppaftp::tokio::AsyncRustlsConnector::from(
            tokio_rustls::TlsConnector::from(Arc::new(config)),
        );
        let domain = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        let domain = domain
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let secured = tokio::time::timeout(STEP, ftp.into_secure(connector, &domain))
            .await
            .map_err(|_| RemoteError::Io(format!("{addr}: no answer to TLS after 30 s")))?;
        ftp = match secured {
            Ok(f) => f,
            Err(e) => {
                let shown = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
                return Err(match shown {
                    Some(fp) => host_key::check_host_key(t.conn.host_key.as_deref(), &fp)
                        .err()
                        .unwrap_or_else(|| RemoteError::Io(e.to_string())),
                    None => RemoteError::Io(format!("TLS: {e}")),
                });
            }
        };
    }
    let (user, pass) = if t.conn.user.is_empty() {
        ("anonymous".to_string(), "anonymous@".to_string())
    } else {
        let pass = match &t.secret {
            Secret::Password { password } => password.clone(),
            _ => String::new(),
        };
        (t.conn.user.clone(), pass)
    };
    timed(&addr, "sign-in", async {
        ftp.login(user.as_str(), pass.as_str())
            .await
            .map_err(|e| match map_err(&addr, e) {
                RemoteError::Io(m) => RemoteError::Auth(m),
                other => other,
            })?;
        ftp.transfer_type(FileType::Binary)
            .await
            .map_err(|e| map_err(&addr, e))
    })
    .await?;
    Ok(ftp)
}

pub struct FtpFs {
    target: Target,
    host: String,
    /// The folder the server signs in to. Paths here are relative to it, so "/" is the user's
    /// own folder even on a server that does not confine them to it.
    home: String,
    /// The server lists with MLSD (from FEAT). Asked once: on FTPS a command the server rejects
    /// leaves the data connection's TLS handshake waiting forever, so trying MLSD and falling
    /// back is not an option there.
    mlsd: bool,
    session: Mutex<Ftp>,
}

/// `path` ("/games/x") under `home` ("/ftp/t") on the server.
pub(crate) fn under(home: &str, path: &str) -> String {
    let home = home.trim_end_matches('/');
    let rest = path.trim_start_matches('/');
    match (home.is_empty(), rest.is_empty()) {
        (true, true) => "/".into(),
        (true, false) => format!("/{rest}"),
        (false, true) => home.to_string(),
        (false, false) => format!("{home}/{rest}"),
    }
}

impl FtpFs {
    pub async fn connect(
        conn: &Connection,
        secret: &Secret,
        tls: bool,
    ) -> Result<Self, RemoteError> {
        let target = Target {
            conn: conn.clone(),
            secret: secret.clone(),
            tls,
        };
        let mut session = sign_in(&target).await?;
        let host = addr_of(conn);
        let home = session.pwd().await.unwrap_or_else(|_| "/".into());
        let mlsd = session
            .feat()
            .await
            .map(|f| {
                f.keys()
                    .any(|k| k.eq_ignore_ascii_case("MLSD") || k.eq_ignore_ascii_case("MLST"))
            })
            .unwrap_or(false);
        crate::log_info!("remote ftp: signed in to {host} tls={tls}");
        Ok(Self {
            target,
            host,
            home,
            mlsd,
            session: Mutex::new(session),
        })
    }

    async fn entries(&self, path: &str) -> Result<Vec<Entry>, RemoteError> {
        let dir = under(&self.home, path);
        let dir = dir.as_str();
        let mut s = self.session.lock().await;
        let host = self.host.as_str();
        let mlsd = self.mlsd;
        timed(host, "a folder listing", async move {
            let listed = if mlsd {
                s.mlsd(Some(dir)).await
            } else {
                s.list(Some(dir)).await
            };
            match listed {
                Ok(lines) => Ok(parse_listing(&lines, mlsd)),
                Err(FtpError::UnexpectedResponse(r)) if r.status.code() == 550 => {
                    Err(RemoteError::NotFound(format!("{dir} not found")))
                }
                Err(e) => Err(map_err(host, e)),
            }
        })
        .await
    }
}

/// A transfer left open mid-file, and where it has got to.
type OpenTransfer = (
    suppaftp::tokio::TransferStream<suppaftp::tokio::AsyncRustlsStream>,
    u64,
);

/// A signed-in session, perhaps partway through reading this file.
struct Session {
    ftp: Ftp,
    open: Option<OpenTransfer>,
}

struct FtpFile {
    target: Target,
    host: String,
    path: String,
    size: u64,
    idle: Mutex<Vec<Session>>,
}

impl FtpFile {
    /// A session to read at `offset`: one whose open transfer is exactly there (reading front
    /// to back keeps one transfer going), else an idle one, else a new sign-in. A session
    /// parked elsewhere in the file is closed rather than aborted — servers answer ABOR in too
    /// many different ways to leave the connection usable afterwards.
    async fn session_at(&self, offset: u64) -> Result<Session, RemoteError> {
        {
            let mut idle = self.idle.lock().await;
            if let Some(i) = idle
                .iter()
                .position(|s| s.open.as_ref().is_some_and(|(_, at)| *at == offset))
            {
                return Ok(idle.swap_remove(i));
            }
            if let Some(i) = idle.iter().position(|s| s.open.is_none()) {
                return Ok(idle.swap_remove(i));
            }
            if idle.len() >= READ_SESSIONS {
                idle.remove(0);
            }
        }
        Ok(Session {
            ftp: sign_in(&self.target).await?,
            open: None,
        })
    }
}

#[async_trait::async_trait]
impl RemoteFile for FtpFile {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if offset >= self.size || len == 0 {
            return Ok(Vec::new());
        }
        let len = len.min(self.size - offset);
        let mut s = self.session_at(offset).await?;
        let mut stream = match s.open.take() {
            Some((stream, _)) => stream,
            None => {
                let (ftp, path, host) = (&mut s.ftp, self.path.as_str(), self.host.as_str());
                timed(host, "a read", async move {
                    ftp.resume_transfer(offset as usize)
                        .await
                        .map_err(|e| map_err(host, e))?;
                    ftp.retr_as_stream(path).await.map_err(|e| map_err(host, e))
                })
                .await?
            }
        };
        let mut out = vec![0u8; len as usize];
        let mut got = 0usize;
        while got < out.len() {
            let n = tokio::time::timeout(STEP, stream.read(&mut out[got..]))
                .await
                .map_err(|_| RemoteError::Io(format!("{}: the read stalled for 30 s", self.host)))?
                .map_err(|e| RemoteError::Io(format!("{}: {e}", self.host)))?;
            if n == 0 {
                break;
            }
            got += n;
        }
        out.truncate(got);
        let at = offset + got as u64;
        if at >= self.size {
            // The whole rest of the file came through: the transfer ends cleanly.
            if stream.finish().await.is_err() {
                return Ok(out);
            }
        } else {
            s.open = Some((stream, at));
        }
        let mut idle = self.idle.lock().await;
        if idle.len() < READ_SESSIONS {
            idle.push(s);
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl RemoteFs for FtpFs {
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
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            return Ok(Entry {
                name: String::new(),
                is_dir: true,
                size: 0,
                mtime: None,
            });
        }
        let (parent, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        let parent = if parent.is_empty() { "/" } else { parent };
        self.entries(parent)
            .await?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| RemoteError::NotFound(format!("{path} not found")))
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        let e = self.stat(path).await?;
        if e.is_dir {
            return Err(RemoteError::NotFound(format!("{path} is a folder")));
        }
        Ok(Arc::new(FtpFile {
            target: self.target.clone(),
            host: self.host.clone(),
            path: under(&self.home, path),
            size: e.size,
            idle: Mutex::new(Vec::new()),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paths_resolve_under_the_login_folder() {
        assert_eq!(under("/", "/games/a.pkg"), "/games/a.pkg");
        assert_eq!(under("/ftp/t", "/games/a.pkg"), "/ftp/t/games/a.pkg");
        assert_eq!(under("/ftp/t/", "/"), "/ftp/t");
        assert_eq!(under("/", "/"), "/");
    }

    #[test]
    fn reads_mlsd_listings() {
        let got = parse_listing(
            &lines(&[
                "type=cdir;modify=20240101000000; .",
                "type=dir;modify=20240101000000; games",
                "type=file;size=1234;modify=20240102030405; a.pkg",
            ]),
            true,
        );
        let names: Vec<_> = got
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir, e.size))
            .collect();
        assert_eq!(names, [("a.pkg", false, 1234), ("games", true, 0)]);
    }

    #[test]
    fn reads_unix_and_windows_list_lines() {
        let unix = parse_listing(
            &lines(&[
                "drwxr-xr-x    2 ftp      ftp          4096 Jan 01 12:00 games",
                "-rw-r--r--    1 ftp      ftp       1048576 Jan 02  2024 My Game.pkg",
            ]),
            false,
        );
        assert_eq!(
            unix.iter()
                .map(|e| (e.name.as_str(), e.is_dir, e.size))
                .collect::<Vec<_>>(),
            [("games", true, 0), ("My Game.pkg", false, 1048576)]
        );
        let dos = parse_listing(
            &lines(&[
                "01-02-24  03:04PM       <DIR>          games",
                "01-02-24  03:04PM              2048 b.exfat",
            ]),
            false,
        );
        assert_eq!(
            dos.iter()
                .map(|e| (e.name.as_str(), e.is_dir, e.size))
                .collect::<Vec<_>>(),
            [("b.exfat", false, 2048), ("games", true, 0)]
        );
    }

    /// FTP: `docker run -d -p 2121:21 -p 21000-21010:21000-21010 -e USERS="t|t" -e ADDRESS=127.0.0.1
    /// -v <dir>:/ftp/t delfer/alpine-ftp-server`, then `PS5UPLOAD_FTP_TEST=127.0.0.1:2121`.
    #[tokio::test]
    #[ignore = "needs an FTP server; see the doc comment"]
    async fn ftp_meets_the_remote_contract() {
        let Ok(addr) = std::env::var("PS5UPLOAD_FTP_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Ftp);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = "t".into();
        let fs = FtpFs::connect(
            &conn,
            &Secret::Password {
                password: "t".into(),
            },
            false,
        )
        .await
        .unwrap();
        crate::remote::contract::check(&fs, "/").await;
    }

    /// FTPS with a self-signed certificate (the delfer image with TLS_CERT/TLS_KEY), then
    /// `PS5UPLOAD_FTPS_TEST=127.0.0.1:2990`. The first connection is refused with the
    /// certificate's fingerprint to accept; once accepted, the server meets the contract.
    #[tokio::test]
    #[ignore = "needs an FTPS server; see the doc comment"]
    async fn ftps_trusts_on_first_use_then_meets_the_contract() {
        let Ok(addr) = std::env::var("PS5UPLOAD_FTPS_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Ftps);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = "t".into();
        let secret = Secret::Password {
            password: "t".into(),
        };
        let fingerprint = match FtpFs::connect(&conn, &secret, true).await {
            Err(RemoteError::HostKey {
                fingerprint,
                changed: false,
            }) => fingerprint,
            Err(e) => panic!("expected an unknown key, got {e}"),
            Ok(_) => panic!("an unknown certificate must not be trusted"),
        };
        assert!(fingerprint.starts_with("SHA256:"));
        conn.host_key = Some(fingerprint);
        let fs = FtpFs::connect(&conn, &secret, true).await.unwrap();
        crate::remote::contract::check(&fs, "/").await;
        conn.host_key = Some("SHA256:someone-else".into());
        assert!(matches!(
            FtpFs::connect(&conn, &secret, true).await,
            Err(RemoteError::HostKey { changed: true, .. })
        ));
    }
}
