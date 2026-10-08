//! What a link actually serves, and a download-only path to a console folder (R4, #368).
//!
//! The link installer used to trust the URL text. That fails the three ways a real link looks:
//! a redirect (`/get?id=…` that answers 302 to a CDN), a share link, and a package whose URL
//! has no `.pkg` in it. So the decision is made from what the origin sends back, after
//! redirects: the first bytes (a PS4 or PS5 package magic) and the content headers.
//!
//! * A link that serves a package installs through the existing paths (`remote_pkg`).
//! * Any other link is "download only", to a console folder, over AVA1: the engine's ranged
//!   fetcher (`RemoteSource`) is the AVA1 source, so nothing is staged on this computer.
//!   It is allowed only for a real file download; an HTML page, an error status, a login page
//!   or an empty body is refused with a message that says which.
//!
//! Android compiles `remote_pkg` out, so this module goes with it (lib.rs answers 501 there).

use std::io::{self, Read};
use std::sync::Arc;

use ava1::source::{ReadAt, Source, SourceMeta};
use serde::Serialize;

use crate::remote_pkg::RemoteSource;

/// The PS4 package magic (`\x7FCNT`): the first four bytes of a PS4 `.pkg`, and of the
/// metadata container inside a PS5 one.
pub const PKG_MAGIC: &[u8; 4] = b"\x7FCNT";
/// The first four bytes of a PS5 `.pkg`: a finalized image (`\x7FFIH`). A fake game package
/// and a homebrew app both start with it. Until 6.4.0 only the PS4 magic counted, so a link
/// to a PS5 package was offered as a plain file download and could not be installed.
pub const PKG_MAGIC_PS5: &[u8; 4] = b"\x7FFIH";

/// How much of the body is read to decide: the magic needs 4 bytes, the HTML sniff a few
/// hundred. Never more, so a 100 GB link costs one small request.
const SNIFF_BYTES: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    /// Serves a PS4/PS5 package: install it.
    Pkg,
    /// Serves some other real file: download it to a console folder.
    File,
    /// Serves something that is not a file download. `reason` says what.
    Refused,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkClass {
    pub kind: LinkKind,
    /// Machine-readable: `html`, `login`, `not_found`, `status`, `empty`, `not_a_file`. Only for `Refused`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// A sentence for the user. Only for `Refused`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The name the file should have: Content-Disposition, else the last segment of the
    /// FINAL url (after redirects). Never empty for a non-refused link.
    pub filename: String,
    pub total_size: Option<u64>,
    /// The origin answered the ranged request with 206, so the parallel fetcher can read it.
    pub ranges: bool,
    pub content_type: String,
}

/// What one probe request returned. Plain data so the decision is a pure function.
#[derive(Debug, Clone, Default)]
pub struct ProbeInput {
    pub status: u16,
    pub content_type: String,
    pub content_disposition: Option<String>,
    /// The total from `Content-Range: bytes 0-4095/N`.
    pub content_range_total: Option<u64>,
    pub content_length: Option<u64>,
    /// The first bytes of the body (at most `SNIFF_BYTES`).
    pub head: Vec<u8>,
    /// The URL the answer came from, after redirects.
    pub final_url: String,
}

fn refused(
    reason: &'static str,
    message: impl Into<String>,
    input: &ProbeInput,
    filename: String,
) -> LinkClass {
    LinkClass {
        kind: LinkKind::Refused,
        reason: Some(reason),
        message: Some(message.into()),
        filename,
        total_size: None,
        ranges: false,
        content_type: input.content_type.clone(),
    }
}

/// Decide from what the origin sent, never from the URL's spelling.
pub fn classify(input: &ProbeInput) -> LinkClass {
    let filename = filename_for(input);
    let status = input.status;
    // Range probes: 416 means the requested range is outside the file, i.e. it is empty.
    if status == 416 {
        return refused(
            "empty",
            "The link serves an empty (zero-length) file, so there is nothing to download.",
            input,
            filename,
        );
    }
    if status == 401 || status == 403 {
        return refused(
            "login",
            format!(
                "The server answered {status}: the link needs a login or is not allowed. \
                 Use a direct download link that works without signing in."
            ),
            input,
            filename,
        );
    }
    if status == 404 || status == 410 {
        return refused(
            "not_found",
            format!("The server answered {status}: nothing is at that link any more."),
            input,
            filename,
        );
    }
    if status >= 400 || !(200..300).contains(&status) {
        return refused(
            "status",
            format!(
                "The server answered {status} instead of sending a file. \
                 The link is wrong, has expired, or the server is down."
            ),
            input,
            filename,
        );
    }

    // The magic decides it: whatever the URL, the type header or the name say.
    if input.head.starts_with(PKG_MAGIC) || input.head.starts_with(PKG_MAGIC_PS5) {
        return LinkClass {
            kind: LinkKind::Pkg,
            reason: None,
            message: None,
            filename,
            total_size: total_of(input),
            ranges: status == 206,
            content_type: input.content_type.clone(),
        };
    }

    let total = total_of(input);
    if total == Some(0) || input.head.is_empty() {
        return refused(
            "empty",
            "The link serves an empty (zero-length) file, so there is nothing to download.",
            input,
            filename,
        );
    }

    let ctype = input
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let attachment = input
        .content_disposition
        .as_deref()
        .is_some_and(|d| d.to_ascii_lowercase().contains("attachment"));
    if ctype == "text/html" || ctype == "application/xhtml+xml" || sniffs_as_html(&input.head) {
        let (reason, message) = if looks_like_login(input) {
            (
                "login",
                "The link opens a login page, not a file. Use a direct download link that \
                 works without signing in.",
            )
        } else {
            (
                "html",
                "The link opens a web page, not a file. Open it in a browser, copy the \
                 real download link, and paste that.",
            )
        };
        return refused(reason, message, input, filename);
    }
    // An API/error body. A genuine .json or .xml file is allowed when the server marks it
    // as a download.
    if !attachment
        && matches!(
            ctype.as_str(),
            "application/json" | "application/xml" | "text/xml" | "application/problem+json"
        )
    {
        return refused(
            "not_a_file",
            format!("The link answered with {ctype} data (an API response), not a file download."),
            input,
            filename,
        );
    }

    LinkClass {
        kind: LinkKind::File,
        reason: None,
        message: None,
        filename,
        total_size: total,
        ranges: status == 206,
        content_type: input.content_type.clone(),
    }
}

fn total_of(input: &ProbeInput) -> Option<u64> {
    input.content_range_total.or(if input.status == 200 {
        input.content_length
    } else {
        None
    })
}

fn sniffs_as_html(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(512)]);
    let t = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    t.starts_with("<!doctype html")
        || t.starts_with("<html")
        || t.starts_with("<head")
        || t.starts_with("<body")
        || t.starts_with("<script")
}

fn looks_like_login(input: &ProbeInput) -> bool {
    let url = input.final_url.to_ascii_lowercase();
    let body = String::from_utf8_lossy(&input.head).to_ascii_lowercase();
    [
        "login", "signin", "sign-in", "log in", "sign in", "password",
    ]
    .iter()
    .any(|w| url.contains(w) || body.contains(w))
}

/// Content-Disposition `filename*=UTF-8''…` / `filename="…"`, else the last path segment of
/// the final URL, percent-decoded and reduced to a console-safe name.
fn filename_for(input: &ProbeInput) -> String {
    let from_header = input
        .content_disposition
        .as_deref()
        .and_then(filename_from_disposition);
    let raw = from_header.unwrap_or_else(|| {
        input
            .final_url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .map(percent_decode)
            .unwrap_or_default()
    });
    let name = sanitize_name(&raw);
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

fn filename_from_disposition(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if let Some(i) = lower.find("filename*=") {
        let rest = &value[i + "filename*=".len()..];
        let rest = rest.split(';').next().unwrap_or("").trim();
        // RFC 5987: charset'lang'percent-encoded
        let enc = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
        let decoded = percent_decode(enc.trim_matches('"'));
        if !decoded.is_empty() {
            return Some(decoded);
        }
    }
    let i = lower.find("filename=")?;
    let rest = value[i + "filename=".len()..].trim_start();
    let v = if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next().unwrap_or("").to_string()
    } else {
        rest.split(';').next().unwrap_or("").trim().to_string()
    };
    (!v.is_empty()).then_some(v)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A name that is one safe path component on the console: no separators, no control
/// characters, no leading dots, bounded length.
pub fn sanitize_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let mut name = cleaned.trim().trim_start_matches('.').trim().to_string();
    while name.len() > 200 {
        name.pop();
    }
    name
}

/// A console destination folder the caller picked: absolute, no `..`, no NUL.
pub fn valid_console_dir(dir: &str) -> Result<String, String> {
    let d = dir.trim();
    if !d.starts_with('/') {
        return Err("the destination folder must be an absolute console path".into());
    }
    if d.contains('\0') || d.split('/').any(|c| c == "..") {
        return Err("the destination folder is not a valid path".into());
    }
    let d = d.trim_end_matches('/');
    if d.is_empty() {
        return Err("choose a folder, not the console's root".into());
    }
    Ok(d.to_string())
}

/// Probe `url` with one small ranged GET and classify what comes back. Blocking.
///
/// `Range: bytes=0-4095` rather than HEAD, for the reason `RemoteSource::probe_with_options`
/// gives: HEAD lies on plenty of hosts. Redirects are followed (ureq's default), and the final
/// URL is read back from the response so a name can come from where the link ended up.
pub fn probe(url: &str, insecure_tls: bool) -> Result<LinkClass, String> {
    use ureq::ResponseExt;
    let agent = crate::remote_pkg::build_agent(1, insecure_tls);
    let mut resp = agent
        .get(url)
        .header("User-Agent", "ps5upload")
        .header("Range", format!("bytes=0-{}", SNIFF_BYTES - 1))
        .call()
        .map_err(|e| format!("could not reach the link: {e}"))?;
    let hdr = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let status = resp.status().as_u16();
    let content_type = hdr("content-type").unwrap_or_default();
    let content_disposition = hdr("content-disposition");
    let content_range_total = hdr("content-range").and_then(|v| {
        v.rsplit('/')
            .next()
            .and_then(|t| t.trim().parse::<u64>().ok())
    });
    let content_length = hdr("content-length").and_then(|v| v.trim().parse::<u64>().ok());
    let final_url = resp.get_uri().to_string();
    let mut head = Vec::new();
    // A 200 to a ranged request streams the whole file: read only the sniff window and drop
    // the connection. Errors reading the body are not fatal to classification (an error page
    // with a broken body is still an error page).
    let _ = resp
        .body_mut()
        .as_reader()
        .take(SNIFF_BYTES)
        .read_to_end(&mut head);
    Ok(classify(&ProbeInput {
        status,
        content_type,
        content_disposition,
        content_range_total,
        content_length,
        head,
        final_url,
    }))
}

/// One remote file as an AVA1 source: reads are ranged GETs through the engine's
/// parallel fetcher, so a download streams link -> AVA1 -> console with no local file.
pub struct LinkSource {
    src: Arc<RemoteSource>,
    name: String,
    size: u64,
}

impl LinkSource {
    pub fn new(src: Arc<RemoteSource>, name: String, size: u64) -> Self {
        Self { src, name, size }
    }
}

struct LinkReader {
    src: Arc<RemoteSource>,
    size: u64,
}

impl ReadAt for LinkReader {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let end = (off + buf.len() as u64).min(self.size) - 1;
        let bytes = self.src.read_range(off, end)?;
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        RemoteSource::prefetch_after(&self.src, off);
        Ok(n)
    }
}

impl Source for LinkSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        if rel != self.name {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_string()));
        }
        Ok(Box::new(LinkReader {
            src: Arc::clone(&self.src),
            size: self.size,
        }))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        if !rel.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_string()));
        }
        Ok(vec![(self.name.clone(), self.stat(&self.name)?)])
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        if rel.is_empty() {
            return Ok(SourceMeta {
                size: 0,
                mtime: 0,
                mode: 0o755,
                is_dir: true,
            });
        }
        if rel != self.name {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_string()));
        }
        Ok(SourceMeta {
            size: self.size,
            mtime: 0,
            mode: 0o644,
            is_dir: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(status: u16, ctype: &str, head: &[u8]) -> ProbeInput {
        ProbeInput {
            status,
            content_type: ctype.into(),
            head: head.to_vec(),
            content_range_total: Some(1000),
            final_url: "https://cdn.example.com/files/blob".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_ps5_package_is_a_package_too() {
        // A PS5 package (a fake game package, a homebrew app) starts with the finalized-image
        // signature, not the PS4 one. Read from a real PS5 homebrew package: 7F 46 49 48.
        let c = classify(&input(
            206,
            "application/octet-stream",
            b"\x7FFIH\x01\0\x03\0",
        ));
        assert_eq!(c.kind, LinkKind::Pkg);
        assert!(c.ranges);
    }

    #[test]
    fn the_package_magic_wins_over_the_url_and_the_type() {
        let c = classify(&input(206, "application/octet-stream", b"\x7FCNT\0\0\0"));
        assert_eq!(c.kind, LinkKind::Pkg);
        assert!(c.ranges);
        // Even a wrong type header does not make a package a web page.
        let c = classify(&input(206, "text/html", b"\x7FCNTxxxx"));
        assert_eq!(c.kind, LinkKind::Pkg);
    }

    #[test]
    fn an_ordinary_file_is_download_only() {
        let c = classify(&input(206, "application/octet-stream", b"PK\x03\x04..."));
        assert_eq!(c.kind, LinkKind::File);
        assert_eq!(c.total_size, Some(1000));
        assert_eq!(c.filename, "blob");
        // text/plain is a legitimate download (a .lua script).
        assert_eq!(
            classify(&input(206, "text/plain", b"print('hi')")).kind,
            LinkKind::File
        );
    }

    #[test]
    fn a_web_page_is_refused_by_type_or_by_body() {
        let c = classify(&input(
            200,
            "text/html; charset=utf-8",
            b"<!DOCTYPE html><html>",
        ));
        assert_eq!((c.kind, c.reason), (LinkKind::Refused, Some("html")));
        // A server that labels a page as octet-stream is still a page.
        let c = classify(&input(
            200,
            "application/octet-stream",
            b"  \n<html><body>x",
        ));
        assert_eq!(c.reason, Some("html"));
    }

    #[test]
    fn a_login_page_is_named_as_one() {
        let mut i = input(200, "text/html", b"<html><form>Please sign in</form>");
        i.final_url = "https://x.example.com/account/login?next=/f".into();
        assert_eq!(classify(&i).reason, Some("login"));
        assert_eq!(
            classify(&input(403, "text/html", b"")).reason,
            Some("login")
        );
        assert_eq!(classify(&input(401, "", b"")).reason, Some("login"));
    }

    #[test]
    fn error_statuses_are_refused() {
        assert_eq!(
            classify(&input(404, "text/html", b"nf")).reason,
            Some("not_found")
        );
        assert_eq!(classify(&input(500, "", b"")).reason, Some("status"));
        // A redirect nobody followed (3xx) is not a file either.
        assert_eq!(classify(&input(302, "", b"")).reason, Some("status"));
    }

    #[test]
    fn an_empty_body_is_refused() {
        let mut i = input(200, "application/octet-stream", b"");
        i.content_range_total = None;
        i.content_length = Some(0);
        assert_eq!(classify(&i).reason, Some("empty"));
        assert_eq!(classify(&input(416, "", b"")).reason, Some("empty"));
        let mut j = input(200, "application/octet-stream", b"");
        j.content_range_total = None;
        assert_eq!(classify(&j).reason, Some("empty"));
    }

    #[test]
    fn an_api_json_error_is_refused_but_a_json_download_is_not() {
        let c = classify(&input(200, "application/json", b"{\"error\":\"nope\"}"));
        assert_eq!(c.reason, Some("not_a_file"));
        let mut i = input(200, "application/json", b"{\"a\":1}");
        i.content_disposition = Some("attachment; filename=\"cfg.json\"".into());
        let c = classify(&i);
        assert_eq!(c.kind, LinkKind::File);
        assert_eq!(c.filename, "cfg.json");
    }

    #[test]
    fn the_name_comes_from_the_header_then_the_final_url() {
        let mut i = input(206, "application/octet-stream", b"MZ..");
        i.content_disposition = Some("attachment; filename*=UTF-8''my%20game.exfat".into());
        assert_eq!(classify(&i).filename, "my game.exfat");
        i.content_disposition = Some("attachment; filename=\"../../evil.elf\"".into());
        assert_eq!(classify(&i).filename, "evil.elf");
        i.content_disposition = None;
        i.final_url = "https://h/p/game%20x.lua?token=abc".into();
        assert_eq!(classify(&i).filename, "game x.lua");
        i.final_url = "https://h/".into();
        assert_eq!(classify(&i).filename, "download");
    }

    #[test]
    fn console_dirs_must_be_absolute_and_plain() {
        assert_eq!(valid_console_dir("/data/dl/").unwrap(), "/data/dl");
        assert!(valid_console_dir("data/x").is_err());
        assert!(valid_console_dir("/data/../etc").is_err());
        assert!(valid_console_dir("/").is_err());
    }

    #[test]
    fn names_are_one_safe_component() {
        assert_eq!(sanitize_name("a/b\\c.bin"), "c.bin");
        assert_eq!(sanitize_name("..hidden"), "hidden");
        assert_eq!(sanitize_name("a:b*c?.txt"), "abc.txt");
        assert_eq!(sanitize_name("   "), "");
    }
}

/// Local HTTP servers for the probe and download tests: a redirect, an extensionless package,
/// an HTML page, a login page, a 404, an empty body and a plain ranged file.
#[cfg(test)]
mod served {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    pub fn pkg_bytes(len: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
        v[..4].copy_from_slice(b"\x7FCNT");
        v
    }

    pub fn file_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8 ^ 0x5a).collect()
    }

    fn respond(
        mut s: TcpStream,
        ranged: Option<(u64, u64)>,
        status: &str,
        ctype: &str,
        extra: &str,
        body: &[u8],
    ) {
        let total = body.len() as u64;
        let (code, slice, cr) = match ranged {
            Some((a, b)) if status == "200 OK" && total > 0 => {
                let b = b.min(total - 1);
                (
                    "206 Partial Content",
                    &body[a as usize..=b as usize],
                    format!("Content-Range: bytes {a}-{b}/{total}\r\n"),
                )
            }
            _ => (status, body, String::new()),
        };
        let head = format!(
            "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{cr}{extra}Connection: close\r\n\r\n",
            slice.len()
        );
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(slice);
    }

    fn handle(mut s: TcpStream, pkg: &[u8], file: &[u8]) {
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let ranged = req
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .map(str::to_string)
            })
            .and_then(|r| {
                let (a, b) = r.trim().split_once('-')?;
                Some((a.parse().ok()?, b.parse().unwrap_or(u64::MAX)))
            });
        match path.split('?').next().unwrap() {
            "/game.pkg" | "/blob" => {
                respond(s, ranged, "200 OK", "application/octet-stream", "", pkg)
            }
            // A share link: redirects (twice) to an extensionless URL that serves a package.
            "/share/abc" => respond(
                s,
                None,
                "302 Found",
                "text/plain",
                "Location: /hop\r\n",
                b"",
            ),
            "/hop" => respond(
                s,
                None,
                "302 Found",
                "text/plain",
                "Location: /blob\r\n",
                b"",
            ),
            "/named" => respond(
                s,
                ranged,
                "200 OK",
                "application/octet-stream",
                "Content-Disposition: attachment; filename=\"Real Game.pkg\"\r\n",
                pkg,
            ),
            "/tool.elf" => respond(s, ranged, "200 OK", "application/octet-stream", "", file),
            "/page" => respond(
                s,
                ranged,
                "200 OK",
                "text/html; charset=utf-8",
                "",
                b"<!DOCTYPE html><html><body>hello</body></html>",
            ),
            "/login" => respond(
                s,
                None,
                "200 OK",
                "text/html",
                "",
                b"<html><form>Please sign in. password:</form></html>",
            ),
            "/missing" => respond(
                s,
                None,
                "404 Not Found",
                "text/html",
                "",
                b"<html>nf</html>",
            ),
            "/empty" => respond(s, None, "200 OK", "application/octet-stream", "", b""),
            _ => respond(s, None, "404 Not Found", "text/plain", "", b"?"),
        }
    }

    /// Serves until the process ends; returns the base URL.
    pub fn serve(pkg: Vec<u8>, file: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let (p, f) = (pkg.clone(), file.clone());
                std::thread::spawn(move || handle(c, &p, &f));
            }
        });
        base
    }
}

#[cfg(test)]
mod served_tests {
    use super::served::*;
    use super::*;

    fn base() -> String {
        serve(pkg_bytes(70_000), file_bytes(5 * 1024 * 1024 + 123))
    }

    #[test]
    fn a_plain_package_link_is_a_package() {
        let b = base();
        let c = probe(&format!("{b}/game.pkg"), false).unwrap();
        assert_eq!(c.kind, LinkKind::Pkg);
        assert_eq!(c.total_size, Some(70_000));
        assert!(c.ranges);
        assert_eq!(c.filename, "game.pkg");
    }

    #[test]
    fn a_share_link_redirecting_to_an_extensionless_package_is_a_package() {
        let b = base();
        let c = probe(&format!("{b}/share/abc"), false).unwrap();
        assert_eq!(
            c.kind,
            LinkKind::Pkg,
            "decided by the bytes after two redirects"
        );
        // The name comes from where the link ended up, not from the URL pasted.
        assert_eq!(c.filename, "blob");
        let c = probe(&format!("{b}/blob"), false).unwrap();
        assert_eq!(c.kind, LinkKind::Pkg);
    }

    #[test]
    fn a_content_disposition_name_wins() {
        let b = base();
        let c = probe(&format!("{b}/named"), false).unwrap();
        assert_eq!(
            (c.kind, c.filename.as_str()),
            (LinkKind::Pkg, "Real Game.pkg")
        );
    }

    #[test]
    fn a_non_package_file_is_download_only() {
        let b = base();
        let c = probe(&format!("{b}/tool.elf"), false).unwrap();
        assert_eq!(c.kind, LinkKind::File);
        assert!(c.ranges);
        assert_eq!(c.total_size, Some(5 * 1024 * 1024 + 123));
        assert_eq!(c.filename, "tool.elf");
    }

    #[test]
    fn pages_errors_and_empty_bodies_are_refused() {
        let b = base();
        let reason = |p: &str| {
            let c = probe(&format!("{b}{p}"), false).unwrap();
            assert_eq!(c.kind, LinkKind::Refused, "{p}");
            assert!(c.message.as_deref().is_some_and(|m| !m.is_empty()));
            c.reason.unwrap()
        };
        assert_eq!(reason("/page"), "html");
        assert_eq!(reason("/login"), "login");
        assert_eq!(reason("/missing"), "not_found");
        assert_eq!(reason("/empty"), "empty");
        assert_eq!(reason("/nothing-here"), "not_found");
    }

    #[test]
    fn the_ranged_fetcher_reads_what_the_server_serves() {
        let b = base();
        let url = format!("{b}/tool.elf");
        let total = 5 * 1024 * 1024 + 123;
        let src = Arc::new(RemoteSource::new_with_options(url, total, false));
        let ls = LinkSource::new(src, "tool.elf".into(), total);
        let mut r = ls.open("tool.elf").unwrap();
        let want = file_bytes(total as usize);
        let mut buf = vec![0u8; 1 << 20];
        let mut off = 0u64;
        loop {
            let n = r.read_at(off, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            assert_eq!(&buf[..n], &want[off as usize..off as usize + n]);
            off += n as u64;
        }
        assert_eq!(off, total);
        assert!(ls.open("other").is_err());
        assert_eq!(ls.stat("tool.elf").unwrap().size, total);
    }

    /// The whole path: ranged link -> LinkSource -> AVA1 upload -> a console folder.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_streams_to_a_console_folder_over_ava1() {
        use ava1::host::FolderHost;
        use ava1::keys::Identity;
        use ava1::peers::PeerStore;
        use ava1::server::{self, ServerCtx};
        use ps5upload_ava1::Pool;

        let b = base();
        let total = 5 * 1024 * 1024 + 123;
        let dir = std::env::temp_dir().join(format!("link-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ava = dir.join("engine");
        let key = Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        std::fs::create_dir_all(dir.join("host/share")).unwrap();
        let mut peers = PeerStore::in_memory();
        peers.add(key, "engine").unwrap();
        let rpc: ava1::server::RpcHandler = Box::new(|_, _| ava1::session::RpcReply {
            status: ava1::gen::ERR_UNKNOWN_METHOD,
            body: Vec::new(),
        });
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc).with_jobs(
            Arc::new(FolderHost {
                root: dir.join("host/share"),
                jobs_dir: dir.join("host/jobs"),
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        let pool = Pool::new(ava).with_addr(addr);

        let url = format!("{b}/tool.elf");
        let r = tokio::task::spawn_blocking(move || {
            let src = Arc::new(RemoteSource::new_with_options(url, total, false));
            let source = Arc::new(LinkSource::new(src, "tool.elf".into(), total));
            let manifest = ava1::manifest::single(source.as_ref(), "tool.elf").unwrap();
            let mut opts = ava1::send::SendOptions::upload("dst/tool.elf");
            opts.flags = ava1::gen::JF_SINGLE_FILE;
            ps5upload_ava1::upload::upload_with_in(
                &pool,
                "127.0.0.1",
                [7; 16],
                manifest,
                source,
                opts,
                &ps5upload_core::transfer::TransferConfig::new("127.0.0.1"),
            )
        })
        .await
        .unwrap()
        .expect("upload");
        assert_eq!(r.bytes_sent, total);
        assert_eq!(
            std::fs::read(dir.join("host/share/dst/tool.elf")).unwrap(),
            file_bytes(total as usize)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
