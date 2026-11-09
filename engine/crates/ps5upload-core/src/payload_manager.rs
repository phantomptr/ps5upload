//! Launch an ELF through itsPLK's Payload Manager (`pldmgr.elf`, HTTP :8084)
//! when the console's ELF loader on :9021 is not answering.
//!
//! The installer daemon can only be started by a loader. :9021 is often gone
//! on consoles set up with Payload Manager and the WebKit Autoloader: the
//! stock elfldr it carries (v0.26) wedges for good once a client connects and
//! sends nothing, and some setups never leave it listening (issues #344,
//! #345). Payload Manager itself stays up and can launch a stored ELF.
//!
//! Its API, read from its own web UI (v0.5.2) and measured on a FW 13.60 Pro,
//! then again against 0.6.1 on a FW 13.60 Phat (#458):
//!   POST /manage:upload?filename=<name>   raw bytes → stored under its folder
//!   GET  /list_payloads                   {"payloads": ["/data/pldmgr/…", …]} and,
//!                                         from 0.6, "items": [{slot, path, kind,
//!                                         filename, name, version, …}]
//!   POST /loadpayload:<path>              launch; only its own folders, any
//!                                         other path is "Invalid payload name".
//!                                         0.6 refuses GET (405) and also takes
//!                                         the slot ("local/<stem>", "repo/<id>@…")
//!   POST /manage:delete?filename=<name>   remove the stored copy (0.6: 405 on GET)
//! 0.5.2 does not look at the method of these two, so POST works on both. 0.6
//! stores an upload at /data/pldmgr/payloads/local/<stem>/<name> (slot
//! "local/<stem>") and its boot autoload list names slots, not file names.
//! The installer answered on :9115 1.1 s after `loadpayload`, beside a running
//! ps5upload helper, and kept running after its stored copy was deleted.
//!
//! Plain HTTP/1.1 over a TcpStream, no client crate: this runs in the Android
//! build too, where the engine has no HTTP client dependency.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Payload Manager's web/API port.
pub const PAYLOAD_MANAGER_PORT: u16 = 8084;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Store `bytes` in Payload Manager as `filename` and launch it. Returns once
/// Payload Manager has accepted the launch; the caller waits for the ELF to
/// come up, then calls [`forget`] so the copy doesn't linger in the user's
/// payload list.
pub fn launch_elf(ip: &str, filename: &str, bytes: &[u8]) -> Result<(), String> {
    let (status, _) = request(ip, "GET", "/version", None)
        .map_err(|e| format!("no Payload Manager on :{PAYLOAD_MANAGER_PORT} ({e})"))?;
    if status != 200 {
        return Err(format!(
            "nothing that looks like Payload Manager on :{PAYLOAD_MANAGER_PORT} (HTTP {status})"
        ));
    }
    // Payload Manager files an upload under a folder it derives from the name,
    // and its delete clears that folder: "elfldr-ps5upload.elf" went into the
    // user's own "elfldr" folder, and cleaning up deleted their elfldr (found
    // on a test console, 2026-09-30). Only ever use a folder that is ours by
    // name — exactly the file's stem, in the "ps5upload-" namespace.
    let (status, body) = request(
        ip,
        "GET",
        &format!("/manage:check?filename={}", encode_query(filename)),
        None,
    )?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not check {filename} (HTTP {status})"
        ));
    }
    own_folder(&body, filename)?;
    let upload = upload_path(filename, &body);
    let (status, body) = request(ip, "POST", &upload, Some(bytes))?;
    if status != 200 {
        return Err(format!(
            "Payload Manager refused the upload (HTTP {status}: {})",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    let (status, body) = request(ip, "GET", "/list_payloads", None)?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not list its payloads (HTTP {status})"
        ));
    }
    let path = stored_path(&body, filename)
        .ok_or_else(|| format!("Payload Manager accepted {filename} but does not list it"))?;
    let (status, body) = request(
        ip,
        "POST",
        &format!("/loadpayload:{}", encode_path(&path)),
        None,
    )?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not launch {filename} (HTTP {status}: {})",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    Ok(())
}

/// Launch the helper through Payload Manager when the loader did not answer. Under our own name,
/// never the file's: Payload Manager files an upload under a folder derived from the name and its
/// cleanup clears that folder, so a user's own "ps5upload" entry must not be the one we land in.
/// A helper that is still running is left to the new one's takeover.
pub fn launch_helper(ip: &str, bytes: &[u8]) -> Result<(), String> {
    const NAME: &str = "ps5upload-helper.elf";
    launch_elf(ip, NAME, bytes)?;
    // Give it a moment to start before the stored copy goes.
    std::thread::sleep(Duration::from_secs(2));
    forget(ip, NAME);
    Ok(())
}

/// Remove the copy [`launch_elf`] stored. Best effort: a leftover entry is
/// only clutter in the user's list, and the launched ELF keeps running.
pub fn forget(ip: &str, filename: &str) {
    let _ = request(
        ip,
        "POST",
        &format!("/manage:delete?filename={}", encode_query(filename)),
        None,
    );
}

/// A payload Payload Manager lists (`GET /list_payloads`), with its sidecar
/// metadata when the file has one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredPayload {
    /// The absolute path on the console; what `/loadpayload:` takes.
    pub path: String,
    /// Payload Manager 0.6's slot (`local/<stem>`, `repo/<id>@<source>`): what its
    /// autoload list names. Empty on 0.5, whose list names files.
    pub slot: String,
    pub filename: String,
    /// The folder Payload Manager keeps it under (`/data/pldmgr/payloads/<folder>/`),
    /// or the mount root for a payload found on a USB stick.
    pub folder: String,
    pub name: String,
    pub version: String,
    pub source: String,
    /// `"repository"`, `"upload"`, … as Payload Manager recorded it.
    pub install_source: String,
}

/// Payload Manager's own version (`GET /version`). Err when nothing answers on :8084.
pub fn version(ip: &str) -> Result<String, String> {
    let (status, body) = request(ip, "GET", "/version", None)
        .map_err(|e| format!("no Payload Manager on :{PAYLOAD_MANAGER_PORT} ({e})"))?;
    if status != 200 {
        return Err(format!(
            "nothing that looks like Payload Manager on :{PAYLOAD_MANAGER_PORT} (HTTP {status})"
        ));
    }
    Ok(String::from_utf8_lossy(&body).trim().to_string())
}

/// Everything Payload Manager lists, its own storage first.
pub fn list(ip: &str) -> Result<Vec<StoredPayload>, String> {
    let (status, body) = request(ip, "GET", "/list_payloads", None)?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not list its payloads (HTTP {status})"
        ));
    }
    parse_list(&body).ok_or_else(|| "Payload Manager's list is not JSON".to_string())
}

fn parse_list(list_json: &[u8]) -> Option<Vec<StoredPayload>> {
    let v: serde_json::Value = serde_json::from_slice(list_json).ok()?;
    let meta = v.get("meta").cloned().unwrap_or(serde_json::Value::Null);
    let text = |m: &serde_json::Value, keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string()
    };
    // 0.6 describes each payload in "items" (and its USB finds in "usb"); 0.5 only
    // lists paths in "payloads" with a "meta" map keyed by file name.
    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
        let usb = v.get("usb").and_then(|u| u.as_array());
        let mut out: Vec<StoredPayload> = items
            .iter()
            .chain(usb.into_iter().flatten())
            .filter_map(|m| {
                let path = m.get("path")?.as_str()?.to_string();
                let filename = text(m, &["filename"]);
                let filename = if filename.is_empty() {
                    path.rsplit('/').next().unwrap_or(&path).to_string()
                } else {
                    filename
                };
                // "local/<stem>" or "repo/<id>@<source>" under its storage; the
                // mount root for a USB find.
                let folder = path
                    .strip_prefix("/data/pldmgr/payloads/")
                    .and_then(|rest| rest.rsplit_once('/').map(|(d, _)| d.to_string()))
                    .or_else(|| path.rsplit_once('/').map(|(d, _)| d.to_string()))
                    .unwrap_or_default();
                Some(StoredPayload {
                    slot: text(m, &["slot"]),
                    name: text(m, &["display_name", "name"]),
                    version: text(m, &["version"]),
                    source: text(m, &["source_name", "source"]),
                    install_source: {
                        let s = text(m, &["install_source", "origin"]);
                        if s.is_empty() {
                            text(m, &["kind"])
                        } else {
                            s
                        }
                    },
                    path,
                    filename,
                    folder,
                })
            })
            .collect();
        out.sort_by_key(|p| !p.path.starts_with("/data/pldmgr/"));
        return Some(out);
    }
    let mut out: Vec<StoredPayload> = v
        .get("payloads")?
        .as_array()?
        .iter()
        .filter_map(|p| p.as_str())
        .map(|path| {
            let filename = path.rsplit('/').next().unwrap_or(path).to_string();
            let folder = path
                .strip_prefix("/data/pldmgr/payloads/")
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    // "/mnt/usb0/x.elf" → "/mnt/usb0"
                    path.rsplit_once('/')
                        .map(|(d, _)| d.to_string())
                        .unwrap_or_default()
                });
            let m = meta
                .get(&filename)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            StoredPayload {
                path: path.to_string(),
                slot: String::new(),
                name: text(&m, &["display_name", "name"]),
                version: text(&m, &["version"]),
                source: text(&m, &["source_name", "source"]),
                install_source: text(&m, &["install_source"]),
                filename,
                folder,
            }
        })
        .collect();
    // Its own storage first, then USB finds; stable within each.
    out.sort_by_key(|p| !p.path.starts_with("/data/pldmgr/"));
    Some(out)
}

/// Launch a payload Payload Manager already lists (`POST /loadpayload:<path>`; 0.6 also
/// takes its slot).
pub fn launch(ip: &str, path: &str) -> Result<(), String> {
    let (status, body) = request(
        ip,
        "POST",
        &format!("/loadpayload:{}", encode_path(path)),
        None,
    )?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not launch {path} (HTTP {status}: {})",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    Ok(())
}

/// Delete a stored payload by file name (`POST /manage:delete`). Payload Manager
/// removes the file and its sidecar (0.6: the slot folder).
pub fn delete(ip: &str, filename: &str) -> Result<(), String> {
    let (status, body) = request(
        ip,
        "POST",
        &format!("/manage:delete?filename={}", encode_query(filename)),
        None,
    )?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not delete {filename} (HTTP {status}: {})",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    Ok(())
}

/// What storing `filename` would touch (`GET /manage:check`): the folder it
/// lands in, and whether that folder or file already exists. An import
/// removes every other file in that folder, so the caller asks the user
/// before storing into one that exists.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreCheck {
    pub folder: String,
    pub folder_exists: bool,
    pub file_exists: bool,
}

pub fn check(ip: &str, filename: &str) -> Result<StoreCheck, String> {
    let (status, body) = request(
        ip,
        "GET",
        &format!("/manage:check?filename={}", encode_query(filename)),
        None,
    )?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not check {filename} (HTTP {status})"
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "Payload Manager's check reply is not JSON".to_string())?;
    Ok(StoreCheck {
        folder: v
            .get("folder_name")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .to_string(),
        folder_exists: v
            .get("folder_exists")
            .and_then(|f| f.as_bool())
            .unwrap_or(false),
        file_exists: v
            .get("file_exists")
            .and_then(|f| f.as_bool())
            .unwrap_or(false),
    })
}

/// Store `bytes` under its real `filename` (`POST /manage:upload`), for the
/// user's own list. Unlike [`launch_elf`] this writes into whatever folder
/// Payload Manager derives from the name, so it is only called after the
/// user confirmed what [`check`] reported.
pub fn store(ip: &str, filename: &str, bytes: &[u8]) -> Result<(), String> {
    if filename.is_empty() || filename.contains('/') || filename.contains("..") {
        return Err(format!("{filename:?} is not a file name"));
    }
    let check = request(
        ip,
        "GET",
        &format!("/manage:check?filename={}", encode_query(filename)),
        None,
    )
    .ok()
    .filter(|(status, _)| *status == 200)
    .map(|(_, body)| body)
    .unwrap_or_default();
    let upload = upload_path(filename, &check);
    let (status, body) = request(ip, "POST", &upload, Some(bytes))?;
    if status != 200 {
        return Err(format!(
            "Payload Manager refused the upload (HTTP {status}: {})",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    Ok(())
}

/// The upload request for `filename`, given Payload Manager's `/manage:check` reply. 0.6.3 refuses
/// an upload over a stored file unless told to replace it (#679), so a copy left by an earlier
/// launch failed every time. Only then does it say so: versions without this check send nothing
/// extra, as before.
fn upload_path(filename: &str, check_json: &[u8]) -> String {
    let path = format!("/manage:upload?filename={}", encode_query(filename));
    let exists = serde_json::from_slice::<serde_json::Value>(check_json)
        .is_ok_and(|v| v.get("file_exists").and_then(|f| f.as_bool()) == Some(true));
    if exists {
        format!("{path}&mode=replace")
    } else {
        path
    }
}

/// Payload Manager's boot autoload: on/off, the seconds it waits after boot,
/// and the ordered list (`/data/pldmgr/autoload.txt`, one entry a line). An
/// entry is a slot (`local/<leaf>`, `repo/<…>`, 0.6), a file name it matches
/// against its stored files, an absolute path under its folders, or `!<ms>`
/// for a pause. Nothing loads at boot while `enabled` is false.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Autoload {
    pub enabled: bool,
    pub delay_s: i64,
    pub list: Vec<String>,
}

pub fn autoload_get(ip: &str) -> Result<Autoload, String> {
    let (status, body) = request(ip, "GET", "/get_config", None)?;
    if status != 200 {
        return Err(format!(
            "Payload Manager could not read its config (HTTP {status})"
        ));
    }
    parse_autoload(&body).ok_or_else(|| "Payload Manager's config is not JSON".to_string())
}

fn parse_autoload(json: &[u8]) -> Option<Autoload> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let list = v
        .get("AUTOLOAD_LIST")
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Some(Autoload {
        enabled: v
            .get("AUTOLOAD_ENABLED")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        delay_s: v
            .get("AUTOLOAD_DELAY")
            .and_then(|d| d.as_i64())
            .unwrap_or(0),
        list,
    })
}

/// Whether Payload Manager keeps `entry` as one line of its boot list. It takes a file name
/// (0.5), a slot like "local/x" or "repo/x@y" (0.6), an absolute path under its folders, or a
/// "!<ms>" pause, and refuses the whole list over a line with a comma or a control character
/// (its `csv_lines`). Only those are refused here: a list read from it is written back as it
/// was, whatever else its entries hold. Refusing more (paths, spaces) once made every write
/// of a list holding such an entry fail, the helper's included (#781).
fn autoload_entry_ok(entry: &str) -> bool {
    let e = entry.trim();
    !e.is_empty() && !e.contains(',') && !e.chars().any(char::is_control)
}

/// Write the autoload switch and list (`POST /set_config`). Payload Manager
/// keeps its other settings when they are not sent.
pub fn autoload_set(ip: &str, enabled: bool, list: &[String]) -> Result<(), String> {
    for entry in list {
        if !autoload_entry_ok(entry) {
            return Err(format!("{entry:?} cannot be an autoload entry"));
        }
    }
    let body = serde_json::json!({
        "AUTOLOAD_ENABLED": enabled,
        "AUTOLOAD_LIST": list.join(","),
    })
    .to_string();
    let (status, resp) = request(ip, "POST", "/set_config", Some(body.as_bytes()))?;
    if status != 200 {
        return Err(format!(
            "Payload Manager refused the config (HTTP {status}: {})",
            String::from_utf8_lossy(&resp).trim()
        ));
    }
    Ok(())
}

/// [`autoload_set`], then the list read back (`GET /get_config`): Err when Payload Manager
/// answered OK but does not hold what was written, so a caller never reports a boot list
/// that is not there (#781). Ok carries what it read back.
pub fn autoload_set_checked(ip: &str, enabled: bool, list: &[String]) -> Result<Autoload, String> {
    autoload_set(ip, enabled, list)?;
    let back =
        autoload_get(ip).map_err(|e| format!("boot list written, but not read back: {e}"))?;
    autoload_matches(&back, enabled, list)?;
    Ok(back)
}

/// Whether `back` is what was written: the same switch and the same entries in order
/// (Payload Manager trims each line and drops empty ones).
fn autoload_matches(back: &Autoload, enabled: bool, list: &[String]) -> Result<(), String> {
    let wrote: Vec<&str> = list
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .collect();
    let read: Vec<&str> = back.list.iter().map(String::as_str).collect();
    if back.enabled != enabled || wrote != read {
        return Err(format!(
            "Payload Manager kept a different boot list: wrote enabled={enabled} [{}], read back enabled={} [{}]",
            wrote.join(","),
            back.enabled,
            read.join(",")
        ));
    }
    Ok(())
}

/// Ok when Payload Manager would store `filename` in a folder of its own that
/// can only be ours: named exactly after the file's stem, in the `ps5upload-`
/// namespace. Anything else could be a folder holding the user's payloads.
fn own_folder(check_json: &[u8], filename: &str) -> Result<(), String> {
    let stem = filename.strip_suffix(".elf").unwrap_or(filename);
    if !stem.starts_with("ps5upload-") {
        return Err(format!(
            "{filename} is not a ps5upload- name; not storing it in Payload Manager"
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(check_json)
        .map_err(|_| "Payload Manager's check reply is not JSON".to_string())?;
    let folder = v.get("folder_name").and_then(|f| f.as_str()).unwrap_or("");
    if folder != stem {
        return Err(format!(
            "Payload Manager would store {filename} in its \"{folder}\" folder, which may hold the user's own payloads; not touching it"
        ));
    }
    Ok(())
}

/// The stored path of `filename` in a `/list_payloads` response.
fn stored_path(list_json: &[u8], filename: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(list_json).ok()?;
    let suffix = format!("/{filename}");
    v.get("payloads")?
        .as_array()?
        .iter()
        .filter_map(|p| p.as_str())
        // Its own folder, never a same-named file on a USB stick.
        .find(|p| p.starts_with("/data/pldmgr/") && p.ends_with(&suffix))
        .map(str::to_string)
}

/// Query-string escaping (what the web UI's encodeURIComponent does).
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Path escaping that keeps `/` (what the web UI's encodeURI does).
fn encode_path(s: &str) -> String {
    s.split('/').map(encode_query).collect::<Vec<_>>().join("/")
}

/// One HTTP/1.1 request with `Connection: close`; returns status and body.
/// The request line and headers. Every request carries Payload Manager's request
/// marker: since 0.6.0 a mutation (`POST`, `manage:delete`, `loadpayload`) without a
/// same-origin `Origin`/`Referer` is refused with 403 "Mutation origin or request
/// marker rejected" unless `X-PLDMGR-Request: 1` is present (its `mutation_authorized`
/// in http_server.c). Older versions ignore the header (#458).
fn request_head(method: &str, path: &str, addr: &str, body_len: usize) -> String {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nX-PLDMGR-Request: 1\r\n"
    );
    if method == "POST" {
        head.push_str(&format!(
            "Content-Type: application/octet-stream\r\nContent-Length: {body_len}\r\n"
        ));
    }
    head.push_str("\r\n");
    head
}

fn request(
    ip: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let addr = crate::payload_lifecycle::join_host_port(ip, PAYLOAD_MANAGER_PORT);
    let body = body.unwrap_or(&[]);
    let head = request_head(method, path, &addr, body.len());
    exchange(&addr, &head, body, CONNECT_TIMEOUT, IO_TIMEOUT)
}

/// One plain HTTP/1.1 request to another small server on the console (`addr` is
/// `ip:port`), `Connection: close`; status and body. For the console's other web payloads
/// (which need no request marker).
pub fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    connect: Duration,
    io: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if method == "POST" {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    exchange(addr, &head, &[], connect, io)
}

fn exchange(
    addr: &str,
    head: &str,
    body: &[u8],
    connect: Duration,
    io: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let sock = std::net::ToSocketAddrs::to_socket_addrs(addr)
        .map_err(|e| format!("resolve {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {addr}: no address"))?;
    let mut stream =
        TcpStream::connect_timeout(&sock, connect).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(io)).ok();
    stream.set_write_timeout(Some(io)).ok();
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| format!("send to {addr}: {e}"))?;
    let mut resp = Vec::new();
    stream
        .read_to_end(&mut resp)
        .map_err(|e| format!("read from {addr}: {e}"))?;
    parse_response(&resp).ok_or_else(|| format!("{addr} sent a response that is not HTTP"))
}

/// Status code and body of a complete `Connection: close` response. Handles a
/// chunked body, which small embedded servers commonly send.
fn parse_response(resp: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&resp[..split]).ok()?;
    let status: u16 = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let raw = &resp[split + 4..];
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    Some((status, if chunked { dechunk(raw)? } else { raw.to_vec() }))
}

fn dechunk(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = raw.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&raw[..eol]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        raw = &raw[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(raw.get(..size)?);
        raw = raw.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_replaces_a_stored_copy_only_when_there_is_one() {
        let there = br#"{"status":"ok","folder_exists":true,"file_exists":true,"folder_name":"ps5upload-helper","existing":{"slot":"local/ps5upload-helper"}}"#;
        assert_eq!(
            upload_path("ps5upload-helper.elf", there),
            "/manage:upload?filename=ps5upload-helper.elf&mode=replace"
        );
        let none = br#"{"status":"ok","folder_exists":false,"file_exists":false,"existing":null}"#;
        assert_eq!(
            upload_path("ps5upload-helper.elf", none),
            "/manage:upload?filename=ps5upload-helper.elf"
        );
        // A version with no check to read.
        assert_eq!(upload_path("a.elf", b""), "/manage:upload?filename=a.elf");
    }

    #[test]
    fn every_request_carries_the_request_marker_payload_manager_0_6_demands() {
        let post = request_head("POST", "/manage:upload?filename=a.elf", "10.0.0.2:8084", 3);
        assert!(post.starts_with("POST /manage:upload?filename=a.elf HTTP/1.1\r\n"));
        assert!(post.contains("\r\nX-PLDMGR-Request: 1\r\n"));
        assert!(post.contains("\r\nContent-Length: 3\r\n"));
        assert!(post.ends_with("\r\n\r\n"));
        let get = request_head("GET", "/manage:delete?filename=a.elf", "10.0.0.2:8084", 0);
        assert!(get.contains("\r\nX-PLDMGR-Request: 1\r\n"));
        assert!(!get.contains("Content-Length"));
    }

    #[test]
    fn finds_the_stored_copy_in_its_own_folder() {
        // The real /list_payloads body from a FW 13.60 Pro, plus the upload.
        let body = br#"{"payloads":["/data/pldmgr/payloads/elfldr/elfldr_v0.26.elf","/mnt/usb0/ps5upload-installer.elf","/data/pldmgr/payloads/ps5upload-installer/ps5upload-installer.elf"],"meta":{}}"#;
        assert_eq!(
            stored_path(body, "ps5upload-installer.elf").as_deref(),
            Some("/data/pldmgr/payloads/ps5upload-installer/ps5upload-installer.elf")
        );
        assert_eq!(stored_path(body, "other.elf"), None);
        assert_eq!(stored_path(b"<!doctype html>", "x.elf"), None);
    }

    #[test]
    fn never_stores_into_a_folder_that_could_be_the_users() {
        // The real mapping: "elfldr-ps5upload.elf" lands in the user's own
        // "elfldr" folder, whose delete removed their elfldr.
        let shared =
            br#"{"status":"ok","folder_exists":true,"file_exists":false,"folder_name":"elfldr"}"#;
        assert!(own_folder(shared, "elfldr-ps5upload.elf").is_err());
        assert!(own_folder(shared, "ps5upload-elfldr.elf").is_err());
        let ours = br#"{"status":"ok","folder_exists":true,"file_exists":false,"folder_name":"ps5upload-installer"}"#;
        assert!(own_folder(ours, "ps5upload-installer.elf").is_ok());
        let fresh = br#"{"status":"ok","folder_exists":false,"file_exists":false,"folder_name":"ps5upload-helper"}"#;
        assert!(own_folder(fresh, "ps5upload-helper.elf").is_ok());
        // A name outside our namespace is refused before anything is asked.
        assert!(own_folder(fresh, "ps5upload.elf").is_err());
    }

    #[test]
    fn lists_stored_payloads_with_their_metadata() {
        let body = br#"{"payloads":["/mnt/usb0/kstuff.elf","/data/pldmgr/payloads/kstuff/kstuff_v1.2.elf"],"meta":{"kstuff_v1.2.elf":{"display_name":"kstuff","version":"v1.2","source_name":"Mirror","install_source":"repository"}}}"#;
        let list = parse_list(body).unwrap();
        assert_eq!(list[0].path, "/data/pldmgr/payloads/kstuff/kstuff_v1.2.elf");
        assert_eq!(list[0].folder, "kstuff");
        assert_eq!(list[0].name, "kstuff");
        assert_eq!(list[0].version, "v1.2");
        assert_eq!(list[0].source, "Mirror");
        assert_eq!(list[1].filename, "kstuff.elf");
        assert_eq!(list[1].folder, "/mnt/usb0");
        assert_eq!(list[1].name, "");
    }

    #[test]
    fn lists_payload_manager_0_6_items_with_their_slots() {
        // Payload Manager 0.6.1 on a FW 13.60 Phat: a repository payload and a
        // web upload. "payloads" is still there but carries no names.
        let body = br#"{"self":{"id":"pldmgr","running_version":"0.6.1","pending_version":""},"items":[{"slot":"repo/elfldr@e8cce5d1","path":"/data/pldmgr/payloads/repo/elfldr@e8cce5d1/elfldr_v0.26.elf","kind":"repo","filename":"elfldr_v0.26.elf","id":"elfldr","source_url":"https://itsplk.github.io/ps5-payloads-mirror/payloads_v2.json","source_name":"Official Repository","name":"elfldr","version":"v0.26","checksum":"ed6d","last_update":"2026-08-29","author":"","details_url":"","source":"https://github.com/ps5-payload-dev/elfldr/releases","description":"An ELF loader"},{"slot":"local/ps5upload-probe","path":"/data/pldmgr/payloads/local/ps5upload-probe/ps5upload-probe.elf","kind":"local","filename":"ps5upload-probe.elf","origin":"web_upload","origin_detail":""}],"usb":[{"slot":"usb/x","path":"/mnt/usb0/x.elf","kind":"usb","filename":"x.elf"}],"payloads":["/data/pldmgr/payloads/repo/elfldr@e8cce5d1/elfldr_v0.26.elf","/data/pldmgr/payloads/local/ps5upload-probe/ps5upload-probe.elf"]}"#;
        let list = parse_list(body).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].slot, "repo/elfldr@e8cce5d1");
        assert_eq!(list[0].folder, "repo/elfldr@e8cce5d1");
        assert_eq!(list[0].name, "elfldr");
        assert_eq!(list[0].version, "v0.26");
        assert_eq!(list[0].source, "Official Repository");
        assert_eq!(list[0].install_source, "repo");
        assert_eq!(list[1].slot, "local/ps5upload-probe");
        assert_eq!(list[1].filename, "ps5upload-probe.elf");
        assert_eq!(list[1].install_source, "web_upload");
        assert_eq!(list[2].folder, "/mnt/usb0");
        assert_eq!(
            stored_path(body, "ps5upload-probe.elf").as_deref(),
            Some("/data/pldmgr/payloads/local/ps5upload-probe/ps5upload-probe.elf")
        );
    }

    #[test]
    fn autoload_entries_are_refused_only_where_payload_manager_refuses_them() {
        // A comma or a control character makes Payload Manager refuse the whole list.
        for bad in ["a,b", "a\rb", "a\nb", "", "  "] {
            assert!(
                autoload_set("127.0.0.9", true, &[bad.into()]).is_err(),
                "{bad:?}"
            );
        }
        // Everything it keeps passes and reaches the network (which fails here): a list read
        // from it holding a path or a spaced name is written back as it was (#781).
        for ok in [
            "repo/elfldr@e8cce5d1",
            "local/ps5upload-probe",
            "!3000",
            "kstuff.elf",
            "/data/pldmgr/payloads/local/kstuff/kstuff.elf",
            "my payload.elf",
        ] {
            let e = autoload_set("127.0.0.9", true, &[ok.into()]).unwrap_err();
            assert!(!e.contains("cannot be an autoload entry"), "{ok}: {e}");
        }
    }

    #[test]
    fn a_boot_list_write_is_checked_against_what_is_read_back() {
        let back = |enabled, list: &[&str]| Autoload {
            enabled,
            delay_s: 5,
            list: list.iter().map(|s| s.to_string()).collect(),
        };
        let wrote: Vec<String> = vec![
            "local/kstuff".into(),
            "!3000".into(),
            "local/ps5upload".into(),
        ];
        assert!(autoload_matches(
            &back(true, &["local/kstuff", "!3000", "local/ps5upload"]),
            true,
            &wrote
        )
        .is_ok());
        // Dropped, reordered, or the switch not kept: an error naming both lists.
        let e =
            autoload_matches(&back(true, &["local/kstuff", "!3000"]), true, &wrote).unwrap_err();
        assert!(
            e.contains("local/ps5upload") && e.contains("read back"),
            "{e}"
        );
        assert!(autoload_matches(
            &back(true, &["local/ps5upload", "!3000", "local/kstuff"]),
            true,
            &wrote
        )
        .is_err());
        assert!(autoload_matches(
            &back(false, &["local/kstuff", "!3000", "local/ps5upload"]),
            true,
            &wrote
        )
        .is_err());
    }

    #[test]
    fn reads_the_autoload_list_in_order() {
        let body = br#"{"AUTOLOAD_ENABLED":true,"AUTOLOAD_LIST":"kstuff.elf,!3000,smp.elf","LAST_REPOSITORY_UPDATE":0,"AUTO_BROWSER_OPEN":false,"AUTOLOAD_DELAY":5,"KILL_DISC_PLAYER_ON_STARTUP":false,"SCAN_USB_PAYLOADS":false,"AUTO_INSTALL_APP":false,"MULTI_SOURCES_ENABLED":false}"#;
        assert_eq!(
            parse_autoload(body).unwrap(),
            Autoload {
                enabled: true,
                delay_s: 5,
                list: vec!["kstuff.elf".into(), "!3000".into(), "smp.elf".into()]
            }
        );
        assert_eq!(
            parse_autoload(br#"{"AUTOLOAD_ENABLED":false,"AUTOLOAD_LIST":""}"#)
                .unwrap()
                .list,
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_autoload_entry_cannot_break_the_list_format() {
        assert!(autoload_set("127.0.0.9", true, &["a,b".into()]).is_err());
        assert!(store("127.0.0.9", "../x.elf", b"").is_err());
    }

    #[test]
    fn escapes_like_the_web_ui() {
        assert_eq!(
            encode_query("ps5upload-installer.elf"),
            "ps5upload-installer.elf"
        );
        assert_eq!(encode_query("a b&c"), "a%20b%26c");
        assert_eq!(
            encode_path("/data/pldmgr/payloads/x y/x y.elf"),
            "/data/pldmgr/payloads/x%20y/x%20y.elf"
        );
    }

    #[test]
    fn parses_plain_and_chunked_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
        assert_eq!(parse_response(plain), Some((200, b"OK".to_vec())));
        let chunked = b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n14\r\nInvalid payload name\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(chunked),
            Some((400, b"Invalid payload name".to_vec()))
        );
        assert_eq!(parse_response(b"garbage"), None);
    }

    #[test]
    fn a_console_without_payload_manager_is_a_clear_error() {
        // Nothing listens on :8084 on this loopback address in the test run.
        let err = launch_elf("127.0.0.9", "x.elf", b"\x7fELF").unwrap_err();
        assert!(err.contains("no Payload Manager"), "{err}");
    }
}

#[cfg(test)]
mod live_tests {
    /// Hardware check: `PS5UPLOAD_LIVE_PM_IP=<console> cargo test -p
    /// ps5upload-core payload_manager::live -- --ignored`. Launches the bundled
    /// installer through the console's Payload Manager, waits for it on :9115,
    /// then removes the stored copy.
    #[test]
    #[ignore]
    fn live_launches_the_installer_through_payload_manager() {
        let Ok(ip) = std::env::var("PS5UPLOAD_LIVE_PM_IP") else {
            return;
        };
        let elf = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../payload/installer/ps5upload-installer.elf"
        ))
        .expect("installer ELF");
        super::launch_elf(&ip, "ps5upload-installer.elf", &elf).expect("launch");
        let addr = format!("{ip}:9115");
        let up = (0..40).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            crate::payload_lifecycle::port_is_open(&addr, std::time::Duration::from_millis(500))
        });
        super::forget(&ip, "ps5upload-installer.elf");
        assert!(up, "installer never answered on {addr}");
    }
}
