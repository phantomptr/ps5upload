//! WebDAV (RFC 4918) over plain HTTP or HTTPS: PROPFIND for listings and stat, ranged GET
//! for reads, and for the organizer's few writes MKCOL, MOVE (`Overwrite: F`) and DELETE of an
//! empty collection. Basic auth, which is what NAS boxes, Nextcloud, Alist and rclone serve, or
//! Digest when the server offers it (see [`super::http_digest`]).
//!
//! Paths are absolute on the server (`/remote.php/dav/files/me/games`): the DAV root goes
//! in the connection's start folder, and the browser walks down from there. Nothing here
//! writes file contents.

use std::sync::Arc;
use std::time::Duration;

use super::http_digest::Auth;
use super::store::{Connection, Protocol, Secret};
use super::{Entry, Page, RemoteError, RemoteFile, RemoteFs, PAGE};

/// One request, including a ranged read of a few MiB from a slow NAS.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// A PUT has no limit on the whole: a game file over a NAS link takes as long as it takes. These
/// bound what can hang: the server taking no bytes, and its answer once it has them all.
const PUT_CONNECT: Duration = Duration::from_secs(30);
const PUT_STALL: Duration = Duration::from_secs(60);
const PUT_COMMIT: Duration = Duration::from_secs(5 * 60);
/// A listing of a folder with thousands of entries is still well under this.
const PROPFIND_MAX: u64 = 64 * 1024 * 1024;

/// A WebDAV server as typed or pasted: `nas`, `nas:5006`, `[fe80::1]:5006`,
/// `https://nas:5006/remote.php/dav`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TypedHost {
    pub host: String,
    pub port: Option<u16>,
    /// What came after the address, with no trailing slash; empty for none.
    pub path: String,
    /// What a typed scheme said: `Some(true)` for https.
    pub https: Option<bool>,
}

pub(crate) fn parse_host(raw: &str) -> TypedHost {
    let raw = raw.trim();
    let mut https = None;
    let mut rest = raw;
    for (scheme, secure) in [
        ("https://", true),
        ("davs://", true),
        ("http://", false),
        ("dav://", false),
    ] {
        // By `get`: a name in Chinese has no letter boundary at a scheme's length (#663).
        if raw
            .get(..scheme.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(scheme))
        {
            https = Some(secure);
            rest = &raw[scheme.len()..];
            break;
        }
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    // user:password@ in a pasted address: the form has its own fields for those.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']')) {
        let (h, after) = authority.split_at(end + 2);
        (h, after.strip_prefix(':').and_then(|p| p.parse().ok()))
    } else {
        match authority.split_once(':') {
            // Exactly one colon: host:port. More than one is a bare IPv6 address.
            Some((h, p)) if !p.contains(':') => match p.parse() {
                Ok(port) => (h, Some(port)),
                Err(_) => (authority, None),
            },
            _ => (authority, None),
        }
    };
    TypedHost {
        host: host.to_string(),
        port: port.filter(|p: &u16| *p != 0),
        path,
        https,
    }
}

fn put_limits(
    b: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    b.timeout_connect(Some(PUT_CONNECT))
        .timeout_per_write(Some(PUT_STALL))
        .timeout_recv_response(Some(PUT_COMMIT))
}

pub struct WebdavFs {
    agent: ureq::Agent,
    /// For PROPFIND and the writes: follows no redirect itself (see [`run_dav`]).
    dav: ureq::Agent,
    /// For a PUT: [`PUT_STALL`] and [`PUT_COMMIT`] instead of [`REQUEST_TIMEOUT`].
    put: ureq::Agent,
    /// `https://nas:5006`, no trailing slash.
    base: String,
    host: String,
    /// How requests are signed, when the connection has a user.
    auth: Arc<Auth>,
}

impl WebdavFs {
    pub async fn connect(conn: &Connection, secret: &Secret) -> Result<Self, RemoteError> {
        let scheme = if conn.protocol == Protocol::Webdavs {
            "https"
        } else {
            "http"
        };
        // Connections saved before the form moved a typed port or path out of Server.
        let typed = parse_host(&conn.host);
        let host = typed.host.as_str();
        let port = typed.port.unwrap_or(conn.port);
        let hostport = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let password = match secret {
            Secret::Password { password } => password.as_str(),
            _ => "",
        };
        let user = conn.user.trim().to_string();
        let auth = if user.is_empty() {
            Auth::None
        } else {
            Auth::basic(&user, password)
        };
        // A certificate the user accepted, as SFTP's host key and FTPS's certificate are. Checked
        // once per connection, then the requests don't verify: ureq has no place for a verifier
        // of our own. Weaker than checking on each request, and far stronger than the
        // verify-nothing switch a self-signed NAS needed before.
        let https = conn.protocol == Protocol::Webdavs;
        let pinned = https && !conn.insecure_tls && conn.host_key.is_some();
        if pinned {
            let shown = super::ftp_fs::certificate_fingerprint(&hostport).await?;
            super::host_key::check_host_key(conn.host_key.as_deref(), &shown)?;
        }
        let unverified = conn.insecure_tls || pinned;
        let builder = |redirects: u32| {
            ureq::Agent::config_builder()
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .disable_verification(unverified)
                        .build(),
                )
                .http_status_as_error(false)
                // PROPFIND is not in ureq's method list.
                .allow_non_standard_methods(true)
                .max_idle_connections_per_host(8)
                .max_redirects(redirects)
        };
        let timed = |redirects: u32| {
            ureq::Agent::new_with_config(
                builder(redirects)
                    .timeout_global(Some(REQUEST_TIMEOUT))
                    .build(),
            )
        };
        let mut fs = Self {
            // A pinned connection doesn't verify, so a redirect could lead reads anywhere.
            agent: timed(if pinned { 0 } else { 10 }),
            dav: timed(0),
            put: ureq::Agent::new_with_config(put_limits(builder(0)).build()),
            base: format!("{scheme}://{hostport}"),
            host: hostport,
            auth: Arc::new(auth),
        };
        // Signing in = one PROPFIND of the start folder (or the root).
        let start = if conn.start_path.trim().is_empty() {
            "/".to_string()
        } else {
            conn.start_path.trim().to_string()
        };
        if !user.is_empty() {
            if let Some(digest) = fs.digest_offered(&start, &user, password).await {
                fs.auth = Arc::new(digest);
            }
        }
        match fs.propfind(&start, 0).await {
            Ok(_) => Ok(fs),
            // A certificate no authority vouches for, as a NAS's usually is: offer it for
            // acceptance. The usual trust is untouched when the server has a proper one.
            Err(RemoteError::Io(m)) if https && !unverified && is_certificate_error(&m) => {
                let shown = super::ftp_fs::certificate_fingerprint(&fs.host).await?;
                Err(RemoteError::HostKey {
                    fingerprint: shown,
                    changed: false,
                })
            }
            Err(e) => Err(e),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, encode_path(path))
    }

    /// PROPFIND `path` with `depth` 0 or 1, parsed.
    /// The connection's Digest form, when an unsigned request says the server offers it.
    async fn digest_offered(&self, path: &str, user: &str, password: &str) -> Option<Auth> {
        let agent = self.dav.clone();
        let url = self.url(path);
        let (user, password) = (user.to_string(), password.to_string());
        tokio::task::spawn_blocking(move || {
            let req = ureq::http::Request::builder()
                .method("PROPFIND")
                .uri(url.as_str())
                .header("Depth", "0")
                .header("User-Agent", "ps5upload")
                .body(())
                .ok()?;
            let resp = agent.run(req).ok()?;
            if resp.status().as_u16() != 401 {
                return None;
            }
            Auth::digest_from(&user, &password, challenges(&resp))
        })
        .await
        .ok()
        .flatten()
    }

    async fn propfind(&self, path: &str, depth: u8) -> Result<Vec<DavItem>, RemoteError> {
        let agent = self.dav.clone();
        let base = self.base.clone();
        let url = self.url(path);
        let auth = self.auth.clone();
        let host = self.host.clone();
        tokio::task::spawn_blocking(move || {
            let mut resp = run_dav(&agent, &base, &url, &host, &auth, |url| {
                let mut req = ureq::http::Request::builder()
                    .method("PROPFIND")
                    .uri(url)
                    .header("Depth", depth.to_string())
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .header("User-Agent", "ps5upload");
                if let Some(a) = auth.header("PROPFIND", url) {
                    req = req.header("Authorization", a);
                }
                req.body(PROPFIND_BODY)
                    .map_err(|e| RemoteError::Io(format!("{host}: {e}")))
            })?;
            let status = resp.status().as_u16();
            match status {
                207 | 200 => {}
                401 | 403 => return Err(RemoteError::Auth(auth_reason(status))),
                404 => return Err(RemoteError::NotFound(format!("{url} not found"))),
                405 | 501 => {
                    return Err(RemoteError::Io(format!(
                        "{host} does not answer PROPFIND (HTTP {status}): not a WebDAV server, or the start folder is not its DAV root"
                    )))
                }
                _ => return Err(RemoteError::Io(format!("{host}: PROPFIND returned HTTP {status}"))),
            }
            let xml = resp
                .body_mut()
                .with_config()
                .limit(PROPFIND_MAX)
                .read_to_string()
                .map_err(|e| RemoteError::Io(format!("{host}: {e}")))?;
            Ok(parse_multistatus(&xml))
        })
        .await
        .map_err(|e| RemoteError::Io(format!("{e}")))?
    }

    /// One bodiless request; the status, with sign-in failures already turned into errors.
    async fn send(
        &self,
        method: &'static str,
        path: &str,
        headers: Vec<(&'static str, String)>,
    ) -> Result<u16, RemoteError> {
        let agent = self.dav.clone();
        let base = self.base.clone();
        let url = self.url(path);
        let auth = self.auth.clone();
        let host = self.host.clone();
        tokio::task::spawn_blocking(move || {
            let resp = run_dav(&agent, &base, &url, &host, &auth, |url| {
                let mut req = ureq::http::Request::builder()
                    .method(method)
                    .uri(url)
                    .header("User-Agent", "ps5upload");
                for (k, v) in &headers {
                    req = req.header(*k, v);
                }
                if let Some(a) = auth.header(method, url) {
                    req = req.header("Authorization", a);
                }
                req.body(())
                    .map_err(|e| RemoteError::Io(format!("{host}: {e}")))
            })?;
            match resp.status().as_u16() {
                s @ (401 | 403) => Err(RemoteError::Auth(auth_reason(s))),
                s => Ok(s),
            }
        })
        .await
        .map_err(|e| RemoteError::Io(format!("{e}")))?
    }

    /// The entries directly under `path`, excluding `path` itself.
    async fn entries(&self, path: &str) -> Result<Vec<Entry>, RemoteError> {
        let me = normal(path);
        let mut out: Vec<Entry> = self
            .propfind(path, 1)
            .await?
            .into_iter()
            .filter(|i| normal(&i.path) != me)
            .map(|i| i.entry())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

/// A response's `WWW-Authenticate` values.
fn challenges<B>(resp: &ureq::http::Response<B>) -> impl Iterator<Item = &str> {
    resp.headers()
        .get_all("WWW-Authenticate")
        .iter()
        .filter_map(|v| v.to_str().ok())
}

/// Sends `make(url)`, and once more to where a redirect points when that is on the same
/// server. ureq's own redirects drop the Authorization header, and Apache (behind many NAS
/// WebDAV shares) redirects a folder's URL without its trailing slash to the one with it.
fn run_dav<B: ureq::AsSendBody>(
    agent: &ureq::Agent,
    base: &str,
    url: &str,
    host: &str,
    auth: &Auth,
    make: impl Fn(&str) -> Result<ureq::http::Request<B>, RemoteError>,
) -> Result<ureq::http::Response<ureq::Body>, RemoteError> {
    let mut resp = agent.run(make(url)?).map_err(|e| map_err(host, e))?;
    if resp.status().as_u16() == 401 && auth.renew(challenges(&resp)) {
        resp = agent.run(make(url)?).map_err(|e| map_err(host, e))?;
    }
    if !matches!(resp.status().as_u16(), 301 | 302 | 307 | 308) {
        return Ok(resp);
    }
    let to = resp
        .headers()
        .get("Location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let next = if to.starts_with('/') {
        format!("{base}{to}")
    } else if to.starts_with(&format!("{base}/")) {
        to.to_string()
    } else {
        return Err(RemoteError::Io(format!(
            "{host} redirected to another server ({to})"
        )));
    };
    agent.run(make(&next)?).map_err(|e| map_err(host, e))
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/><D:getcontentlength/><D:getlastmodified/></D:prop></D:propfind>"#;

/// Whether a failed request's words say the server's certificate wasn't trusted.
fn is_certificate_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("certificate") || m.contains("unknownissuer") || m.contains("unknown issuer")
}

/// Pieces queued between a WebDAV writer and its PUT.
const PUT_QUEUE: usize = 4;

/// What a writer hands its PUT: the next bytes, or the end of the body.
enum BodyPiece {
    Data(Vec<u8>),
    End,
}

/// A PUT's body, read on the request's thread from what the writer sends. The writer gone
/// without saying the body ended is an error, so a given-up write never looks complete.
struct BodyReader {
    rx: tokio::sync::mpsc::Receiver<BodyPiece>,
    piece: Vec<u8>,
    at: usize,
    ended: bool,
}

impl std::io::Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.piece.len() {
            if self.ended {
                return Ok(0);
            }
            match self.rx.blocking_recv() {
                Some(BodyPiece::Data(d)) => {
                    self.piece = d;
                    self.at = 0;
                }
                Some(BodyPiece::End) => self.ended = true,
                None => return Err(std::io::Error::other("cancelled")),
            }
        }
        let n = buf.len().min(self.piece.len() - self.at);
        buf[..n].copy_from_slice(&self.piece[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A file being written by one streamed PUT.
struct WebdavWriter {
    tx: Option<tokio::sync::mpsc::Sender<BodyPiece>>,
    job: tokio::task::JoinHandle<Result<(), RemoteError>>,
    path: String,
}

impl WebdavWriter {
    /// The PUT ended before its body did: its own failure says why.
    async fn ended_early(&mut self) -> RemoteError {
        self.tx = None;
        match (&mut self.job).await {
            Ok(Err(e)) => e,
            Ok(Ok(())) => RemoteError::Io(format!(
                "the server answered before {} was all sent",
                self.path
            )),
            Err(e) => RemoteError::Io(format!("{e}")),
        }
    }
}

#[async_trait::async_trait]
impl super::RemoteWriter for WebdavWriter {
    async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError> {
        let Some(tx) = &self.tx else {
            return Err(RemoteError::Io(format!("{} was given up", self.path)));
        };
        if tx.send(BodyPiece::Data(data.to_vec())).await.is_err() {
            return Err(self.ended_early().await);
        }
        Ok(())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), RemoteError> {
        if let Some(tx) = self.tx.take() {
            if tx.send(BodyPiece::End).await.is_err() {
                return Err(self.ended_early().await);
            }
        }
        match (&mut self.job).await {
            Ok(r) => r,
            Err(e) => Err(RemoteError::Io(format!("{e}"))),
        }
    }

    /// The body ends in an error, so the PUT fails rather than store a short file.
    async fn abort(mut self: Box<Self>) {
        self.tx = None;
        let _ = (&mut self.job).await;
    }
}

fn auth_reason(status: u16) -> String {
    if status == 403 {
        "the server refused this user (HTTP 403)".into()
    } else {
        "the server rejected the user and password (HTTP 401). Basic and Digest authentication are supported".into()
    }
}

fn map_err(host: &str, e: ureq::Error) -> RemoteError {
    let s = e.to_string();
    let l = s.to_ascii_lowercase();
    if l.contains("certificate") || l.contains("tls") || l.contains("handshake") {
        RemoteError::Io(format!("{host}: {s}"))
    } else if l.contains("timed out") || l.contains("timeout") {
        RemoteError::Io(format!("{host}: timed out"))
    } else if l.contains("connect")
        || l.contains("refused")
        || l.contains("resolve")
        || l.contains("dns")
        || l.contains("unreachable")
    {
        RemoteError::Unreachable(format!("{host}: {s}"))
    } else {
        RemoteError::Io(format!("{host}: {s}"))
    }
}

/// `/a b/c` → `/a%20b/c`: path characters that are not safe in a URL are percent-encoded.
pub(crate) fn encode_path(path: &str) -> String {
    let p = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let mut out = String::with_capacity(p.len() + 8);
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn decode_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // `get` rather than a slice: a literal `%` followed by a multibyte character
            // would otherwise cut inside the character and panic.
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path with one leading slash and no trailing slash ("" for the root).
pub(crate) fn normal(p: &str) -> String {
    let t = p.trim().trim_end_matches('/');
    if t.is_empty() {
        String::new()
    } else if t.starts_with('/') {
        t.to_string()
    } else {
        format!("/{t}")
    }
}

/// One `<response>` of a multistatus body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DavItem {
    /// Decoded, normalised server path.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<i64>,
}

impl DavItem {
    fn entry(&self) -> Entry {
        Entry {
            name: self.path.rsplit('/').next().unwrap_or("").to_string(),
            is_dir: self.is_dir,
            size: if self.is_dir { 0 } else { self.size },
            mtime: self.mtime,
        }
    }
}

/// The text between `<[prefix:]name ...>` and `</[prefix:]name>`, prefix-agnostic. An
/// empty element (`<D:collection/>`) yields `Some("")`.
fn element<'a>(xml: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let lower = xml.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let mut from = 0;
    while let Some(lt) = lower[from..].find('<') {
        let start = from + lt;
        let rest = &lower[start + 1..];
        // Skip closing tags, comments and processing instructions.
        if rest.starts_with('/') || rest.starts_with('!') || rest.starts_with('?') {
            from = start + 1;
            continue;
        }
        let tag_end = rest
            .find(|c: char| c == '>' || c.is_whitespace() || c == '/')
            .unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let local = tag.rsplit(':').next().unwrap_or(tag);
        if local == name {
            let gt = rest.find('>')?;
            let self_closing = rest[..gt].ends_with('/');
            let body_start = start + 1 + gt + 1;
            if self_closing {
                return Some((&xml[body_start..body_start], &xml[body_start..]));
            }
            // The matching close tag: the first `</…name>` after the body (no nesting of
            // same-named elements in a PROPFIND body).
            let close = find_close(&lower[body_start..], &name)?;
            let body_end = body_start + close;
            let after = lower[body_end..].find('>').map(|g| body_end + g + 1)?;
            return Some((&xml[body_start..body_end], &xml[after..]));
        }
        from = start + 1;
    }
    None
}

fn find_close(lower: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(lt) = lower[from..].find("</") {
        let at = from + lt;
        let rest = &lower[at + 2..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = rest[..end].trim();
        if tag.rsplit(':').next().unwrap_or(tag) == name {
            return Some(at);
        }
        from = at + 2;
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Every `<response>` in a 207 body. Tolerant of any namespace prefix and of servers that
/// put an absolute URL in `href`.
pub(crate) fn parse_multistatus(xml: &str) -> Vec<DavItem> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some((body, after)) = element(rest, "response") {
        rest = after;
        let Some((href, _)) = element(body, "href") else {
            continue;
        };
        let href = unescape(href.trim());
        // `http://host/a/b` → `/a/b`; `/a/b` stays.
        let path_part = match href.find("://") {
            Some(i) => href[i + 3..]
                .find('/')
                .map(|j| &href[i + 3 + j..])
                .unwrap_or("/"),
            None => href.as_str(),
        };
        let path = normal(&decode_percent(path_part));
        let is_dir = element(body, "resourcetype")
            .map(|(rt, _)| element(rt, "collection").is_some())
            .unwrap_or(false)
            || href.ends_with('/');
        let size = element(body, "getcontentlength")
            .and_then(|(s, _)| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let mtime = element(body, "getlastmodified").and_then(|(s, _)| parse_http_date(s.trim()));
        out.push(DavItem {
            path,
            is_dir,
            size,
            mtime,
        });
    }
    out
}

/// `Sat, 11 Oct 2026 09:00:00 GMT` → Unix seconds. Only the RFC 1123 form servers send.
pub(crate) fn parse_http_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let (day, mon, year, hms) = (parts[1], parts[2], parts[3], parts[4]);
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| m.eq_ignore_ascii_case(mon))? as i64
        + 1;
    let mut t = hms.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let m: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let (y, mo) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * mo + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + m * 60 + sec)
}

struct WebdavFile {
    fs: Arc<WebdavInner>,
    path: String,
    size: u64,
}

/// What a file handle needs from the session (cloned out so handles outlive the listing).
struct WebdavInner {
    agent: ureq::Agent,
    base: String,
    host: String,
    auth: Arc<Auth>,
}

#[async_trait::async_trait]
impl RemoteFile for WebdavFile {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if offset >= self.size || len == 0 {
            return Ok(Vec::new());
        }
        let len = len.min(self.size - offset);
        let end = offset + len - 1;
        let inner = Arc::clone(&self.fs);
        let url = format!("{}{}", inner.base, encode_path(&self.path));
        tokio::task::spawn_blocking(move || {
            let send = || {
                let mut req = inner
                    .agent
                    .get(&url)
                    .header("User-Agent", "ps5upload")
                    .header("Range", format!("bytes={offset}-{end}"));
                if let Some(a) = inner.auth.header("GET", &url) {
                    req = req.header("Authorization", a);
                }
                req.call().map_err(|e| map_err(&inner.host, e))
            };
            let mut resp = send()?;
            if resp.status().as_u16() == 401 && inner.auth.renew(challenges(&resp)) {
                resp = send()?;
            }
            match resp.status().as_u16() {
                206 => {}
                // The server ignored the range: usable only from the start, and only up to len.
                200 if offset == 0 => {}
                200 => {
                    return Err(RemoteError::Io(format!(
                        "{} does not support ranged reads (HTTP 200 for a Range request)",
                        inner.host
                    )))
                }
                401 | 403 => return Err(RemoteError::Auth(auth_reason(resp.status().as_u16()))),
                404 => return Err(RemoteError::NotFound(format!("{url} not found"))),
                s => {
                    return Err(RemoteError::Io(format!(
                        "{}: GET returned HTTP {s}",
                        inner.host
                    )))
                }
            }
            // Read exactly the bytes asked for; a 200 body (whole file) is cut at `len`.
            use std::io::Read;
            let mut buf = Vec::with_capacity(len as usize);
            resp.body_mut()
                .as_reader()
                .take(len)
                .read_to_end(&mut buf)
                .map_err(|e| RemoteError::Io(format!("{}: {e}", inner.host)))?;
            Ok(buf)
        })
        .await
        .map_err(|e| RemoteError::Io(format!("{e}")))?
    }
}

#[async_trait::async_trait]
impl RemoteFs for WebdavFs {
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
        let me = normal(path);
        let items = self.propfind(path, 0).await?;
        let item = items
            .into_iter()
            .find(|i| normal(&i.path) == me)
            .ok_or_else(|| RemoteError::NotFound(format!("{path} not found")))?;
        Ok(Entry {
            name: if me.is_empty() {
                String::new()
            } else {
                item.entry().name
            },
            ..item.entry()
        })
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        let e = self.stat(path).await?;
        if e.is_dir {
            return Err(RemoteError::NotFound(format!("{path} is a folder")));
        }
        Ok(Arc::new(WebdavFile {
            fs: Arc::new(WebdavInner {
                agent: self.agent.clone(),
                base: self.base.clone(),
                host: self.host.clone(),
                auth: Arc::clone(&self.auth),
            }),
            path: normal(path),
            size: e.size,
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
        Ok(out)
    }

    async fn create_dir(&self, path: &str) -> Result<(), RemoteError> {
        match self.send("MKCOL", path, Vec::new()).await? {
            200 | 201 | 204 => Ok(()),
            // 405: something is there already. A folder is fine; a file is not.
            405 => match self.stat(path).await {
                Ok(e) if e.is_dir => Ok(()),
                Ok(_) => Err(RemoteError::Exists(format!("{path} is a file"))),
                Err(e) => Err(e),
            },
            409 => Err(RemoteError::NotFound(format!("no folder for {path}"))),
            s => Err(RemoteError::Io(format!(
                "{}: MKCOL {path} returned HTTP {s}",
                self.host
            ))),
        }
    }

    /// MOVE with `Overwrite: F`: the server answers 412 rather than replace the target.
    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        let dest = self.url(&normal(to));
        let headers = vec![("Destination", dest), ("Overwrite", "F".to_string())];
        match self.send("MOVE", &normal(from), headers).await? {
            200 | 201 | 204 => Ok(()),
            412 => Err(RemoteError::Exists(format!("{to} already exists"))),
            404 => Err(RemoteError::NotFound(format!("{from} not found"))),
            409 => Err(RemoteError::NotFound(format!("no folder for {to}"))),
            s => Err(RemoteError::Io(format!(
                "{}: MOVE {from} returned HTTP {s}",
                self.host
            ))),
        }
    }

    /// One PUT of the whole file, streamed with its Content-Length from what the writer is
    /// given. No redirect is followed (the body is read once). Always from the start: a PUT
    /// replaces the file whole.
    async fn create_writer<'a>(
        &'a self,
        path: &str,
        size: u64,
        from: u64,
    ) -> Result<Box<dyn super::RemoteWriter + 'a>, RemoteError> {
        if from != 0 {
            return Err(RemoteError::Unsupported(format!(
                "WebDAV writes {path} whole"
            )));
        }
        // A body is too much to send twice: this renews a Digest nonce that has aged out first.
        let _ = self
            .propfind(path.rsplit_once('/').map_or("/", |(dir, _)| dir), 0)
            .await;
        let agent = self.put.clone();
        let url = self.url(&normal(path));
        let auth = self.auth.clone();
        let host = self.host.clone();
        let shown = path.to_string();
        let (tx, rx) = tokio::sync::mpsc::channel::<BodyPiece>(PUT_QUEUE);
        let job = tokio::task::spawn_blocking(move || {
            let mut body = BodyReader {
                rx,
                piece: Vec::new(),
                at: 0,
                ended: false,
            };
            let mut req = ureq::http::Request::builder()
                .method("PUT")
                .uri(&url)
                .header("User-Agent", "ps5upload")
                .header("Content-Length", size.to_string());
            if let Some(a) = auth.header("PUT", &url) {
                req = req.header("Authorization", a);
            }
            let req = req
                .body(ureq::SendBody::from_reader(&mut body))
                .map_err(|e| RemoteError::Io(format!("{host}: {e}")))?;
            let resp = agent.run(req).map_err(|e| map_err(&host, e))?;
            match resp.status().as_u16() {
                200 | 201 | 204 => Ok(()),
                s @ (401 | 403) => Err(RemoteError::Auth(auth_reason(s))),
                409 => Err(RemoteError::NotFound(format!("no folder for {shown}"))),
                s => Err(RemoteError::Io(format!(
                    "{host}: PUT {shown} returned HTTP {s}"
                ))),
            }
        });
        Ok(Box::new(WebdavWriter {
            tx: Some(tx),
            job,
            path: path.to_string(),
        }))
    }
    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        match self.send("DELETE", &normal(path), Vec::new()).await? {
            200 | 202 | 204 | 404 => Ok(()),
            s => Err(RemoteError::Io(format!(
                "{}: DELETE {path} returned HTTP {s}",
                self.host
            ))),
        }
    }

    /// Never: DELETE on a collection removes it with everything in it, so even after a
    /// listing showed it empty, a file landing in the moment before the DELETE would go too.
    /// An emptied folder is left in place.
    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unsupported(format!(
            "{path} left in place: WebDAV can only remove a folder with everything in it"
        )))
    }
}

#[cfg(test)]
mod typed_host_tests {
    use super::*;

    fn typed(host: &str, port: Option<u16>, path: &str, https: Option<bool>) -> TypedHost {
        TypedHost {
            host: host.into(),
            port,
            path: path.into(),
            https,
        }
    }

    #[test]
    fn a_typed_port_scheme_and_path_come_out_of_the_server_field() {
        assert_eq!(parse_host(" nas "), typed("nas", None, "", None));
        assert_eq!(parse_host("nas:5006"), typed("nas", Some(5006), "", None));
        assert_eq!(
            parse_host("https://nas:5006/remote.php/dav/"),
            typed("nas", Some(5006), "/remote.php/dav", Some(true))
        );
        assert_eq!(
            parse_host("HTTP://me:pw@nas/dav"),
            typed("nas", None, "/dav", Some(false))
        );
        assert_eq!(
            parse_host("192.168.1.10:8080"),
            typed("192.168.1.10", Some(8080), "", None)
        );
    }

    #[test]
    fn ipv6_addresses_keep_their_colons() {
        assert_eq!(parse_host("fe80::1"), typed("fe80::1", None, "", None));
        assert_eq!(parse_host("[fe80::1]"), typed("[fe80::1]", None, "", None));
        assert_eq!(
            parse_host("https://[fe80::1]:5006/dav"),
            typed("[fe80::1]", Some(5006), "/dav", Some(true))
        );
    }

    #[test]
    fn a_name_in_chinese_does_not_panic() {
        assert_eq!(parse_host("群晖:5006"), typed("群晖", Some(5006), "", None));
    }

    #[test]
    fn a_put_has_no_limit_on_the_whole_request() {
        // 120 s on the whole request cut every file that takes longer to send.
        let config = put_limits(ureq::Agent::config_builder()).build();
        assert_eq!(config.timeouts().global, None);
        assert_eq!(config.timeout_per_write(), Some(PUT_STALL));
        assert_eq!(config.timeouts().recv_response, Some(PUT_COMMIT));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untrusted_certificate_is_told_from_other_failures() {
        assert!(is_certificate_error(
            "nas:5006: invalid peer certificate: UnknownIssuer"
        ));
        assert!(is_certificate_error("nas: certificate expired"));
        assert!(!is_certificate_error("nas: connection refused"));
    }

    const NEXTCLOUD: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns">
 <d:response>
  <d:href>/remote.php/dav/files/me/games/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getlastmodified>Sat, 11 Oct 2026 09:00:00 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontentlength/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response>
  <d:href>/remote.php/dav/files/me/games/Jak%20X/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response>
  <d:href>http://nas/remote.php/dav/files/me/games/game%20&amp;%20more.pkg</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>12345</d:getcontentlength><d:getlastmodified>Thu, 01 Jan 1970 00:00:10 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;

    #[test]
    fn parses_a_nextcloud_listing_with_any_prefix_and_absolute_hrefs() {
        let items = parse_multistatus(NEXTCLOUD);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].path, "/remote.php/dav/files/me/games");
        assert!(items[0].is_dir);
        assert_eq!(
            items[0].mtime,
            parse_http_date("Sat, 11 Oct 2026 09:00:00 GMT")
        );
        assert_eq!(items[1].path, "/remote.php/dav/files/me/games/Jak X");
        assert!(items[1].is_dir);
        assert_eq!(
            items[2].path,
            "/remote.php/dav/files/me/games/game & more.pkg"
        );
        assert!(!items[2].is_dir);
        assert_eq!(items[2].size, 12345);
        assert_eq!(items[2].mtime, Some(10));
        // No prefix at all, uppercase tags, and a self-closing resourcetype.
        let plain = r#"<multistatus xmlns="DAV:"><RESPONSE><HREF>/x/y.bin</HREF><propstat><prop><RESOURCETYPE/><getcontentlength>7</getcontentlength></prop></propstat></RESPONSE></multistatus>"#;
        let p = parse_multistatus(plain);
        assert_eq!(p[0].path, "/x/y.bin");
        assert!(!p[0].is_dir);
        assert_eq!(p[0].size, 7);
    }

    #[test]
    fn http_dates_become_unix_seconds() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Sat, 11 Oct 2026 09:00:00 GMT"),
            Some(1_791_709_200)
        );
        assert_eq!(parse_http_date("nonsense"), None);
    }

    #[test]
    fn paths_round_trip_through_the_url() {
        assert_eq!(
            encode_path("/games/Jak X/a&b.pkg"),
            "/games/Jak%20X/a%26b.pkg"
        );
        assert_eq!(
            decode_percent("/games/Jak%20X/a%26b.pkg"),
            "/games/Jak X/a&b.pkg"
        );
        assert_eq!(decode_percent("100%"), "100%");
        assert_eq!(normal("games/"), "/games");
        assert_eq!(normal("/"), "");
    }

    /// A loopback server that speaks just enough WebDAV: PROPFIND of `/d/` lists two files,
    /// GET honours Range. Proves the request shape and the read path end to end.
    #[tokio::test]
    async fn lists_and_reads_from_a_loopback_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let served = data.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut s = stream.unwrap();
                let mut buf = vec![0u8; 8192];
                let mut got = 0;
                let (head_len, body_len) = loop {
                    let n = s.read(&mut buf[got..]).unwrap();
                    if n == 0 {
                        break (got, 0);
                    }
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_ascii_lowercase();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let cl = text
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, cl);
                    }
                };
                while got < head_len + body_len {
                    let n = s.read(&mut buf[got..]).unwrap();
                    if n == 0 {
                        break;
                    }
                    got += n;
                }
                // Header names arrive lowercased from the http crate; match case-insensitively.
                let text = String::from_utf8_lossy(&buf[..head_len]).to_ascii_lowercase();
                let line = text.lines().next().unwrap_or("").to_string();
                let authed =
                    text.contains("authorization: basic dTpw".to_ascii_lowercase().as_str()); // u:p
                let resp: Vec<u8> = if !authed {
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                } else if line.starts_with("propfind /d") {
                    let body = r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/d/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response><D:response><D:href>/d/a%20b.bin</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>1000</D:getcontentlength></D:prop></D:propstat></D:response><D:response><D:href>/d/sub/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response></D:multistatus>"#;
                    format!("HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
                } else if line.starts_with("get /d/a%20b.bin") {
                    let range = text
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .map(|r| {
                            let (a, b) = r.trim().split_once('-').unwrap();
                            (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap())
                        });
                    match range {
                        Some((a, b)) => {
                            let slice = &served[a..=b.min(served.len() - 1)];
                            let mut r = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", a + slice.len() - 1, served.len(), slice.len()).into_bytes();
                            r.extend_from_slice(slice);
                            r
                        }
                        None => b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    }
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                s.write_all(&resp).unwrap();
            }
        });
        let conn = Connection {
            id: "t".into(),
            name: "t".into(),
            protocol: Protocol::Webdav,
            host: "127.0.0.1".into(),
            port,
            share: String::new(),
            user: "u".into(),
            start_path: "/d".into(),
            from_root: false,
            host_key: None,
            insecure_tls: false,
        };
        // Wrong password: a clear sign-in error, not a listing.
        let bad = WebdavFs::connect(
            &conn,
            &Secret::Password {
                password: "x".into(),
            },
        )
        .await
        .err();
        assert!(matches!(bad, Some(RemoteError::Auth(_))), "{bad:?}");
        let fs = WebdavFs::connect(
            &conn,
            &Secret::Password {
                password: "p".into(),
            },
        )
        .await
        .unwrap();
        let page = fs.list("/d", None).await.unwrap();
        let names: Vec<_> = page
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir))
            .collect();
        assert_eq!(names, vec![("a b.bin", false), ("sub", true)]);
        let f = fs.open("/d/a b.bin").await.unwrap();
        assert_eq!(f.size(), 1000);
        assert_eq!(f.read_at(10, 5).await.unwrap(), data[10..15]);
        assert_eq!(f.read_at(995, 50).await.unwrap(), data[995..]);
        assert!(f.read_at(1000, 5).await.unwrap().is_empty());
        let walked = fs.walk("/d", 10).await.unwrap();
        assert!(walked.iter().any(|(p, _)| p == "a b.bin"));
    }

    /// A real server: `docker run -d -p 8088:80 -e AUTH_TYPE=Basic -e USERNAME=t -e PASSWORD=t
    /// bytemark/webdav` with files `w/a` and `w/b` of different sizes in its /var/lib/dav/data,
    /// then `PS5UPLOAD_WEBDAV_TEST=127.0.0.1:8088`.
    #[tokio::test]
    #[ignore = "needs a WebDAV server; see the doc comment"]
    async fn webdav_meets_the_remote_contract() {
        let Ok(addr) = std::env::var("PS5UPLOAD_WEBDAV_TEST") else {
            return;
        };
        let (host, port) = addr.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", Protocol::Webdav);
        conn.host = host.into();
        conn.port = port.parse().unwrap();
        conn.user = "t".into();
        let fs = WebdavFs::connect(
            &conn,
            &Secret::Password {
                password: "t".into(),
            },
        )
        .await
        .unwrap();
        crate::remote::contract::check(&fs, "/").await;
        crate::remote::contract::check_writes(&fs, "/w").await;
    }
}
