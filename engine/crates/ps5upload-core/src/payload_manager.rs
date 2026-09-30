//! Launch an ELF through itsPLK's Payload Manager (`pldmgr.elf`, HTTP :8084)
//! when the console's ELF loader on :9021 is not answering.
//!
//! The installer daemon can only be started by a loader. :9021 is often gone
//! on consoles set up with Payload Manager and the WebKit Autoloader: the
//! stock elfldr it carries (v0.26) wedges for good once a client connects and
//! sends nothing, and some setups never leave it listening (issues #344,
//! #345). Payload Manager itself stays up and can launch a stored ELF.
//!
//! Its API, read from its own web UI (v0.5.2) and measured on a FW 13.60 Pro:
//!   POST /manage:upload?filename=<name>   raw bytes → stored under its folder
//!   GET  /list_payloads                   {"payloads": ["/data/pldmgr/…", …]}
//!   GET  /loadpayload:<path>              launch; only its own folders, any
//!                                         other path is "Invalid payload name"
//!   GET  /manage:delete?filename=<name>   remove the stored copy
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
    let upload = format!("/manage:upload?filename={}", encode_query(filename));
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
        "GET",
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

/// Remove the copy [`launch_elf`] stored. Best effort: a leftover entry is
/// only clutter in the user's list, and the launched ELF keeps running.
pub fn forget(ip: &str, filename: &str) {
    let _ = request(
        ip,
        "GET",
        &format!("/manage:delete?filename={}", encode_query(filename)),
        None,
    );
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
fn encode_query(s: &str) -> String {
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
fn request(
    ip: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let addr = crate::payload_lifecycle::join_host_port(ip, PAYLOAD_MANAGER_PORT);
    let sock = std::net::ToSocketAddrs::to_socket_addrs(&addr)
        .map_err(|e| format!("resolve {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {addr}: no address"))?;
    let mut stream = TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT)
        .map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let body = body.unwrap_or(&[]);
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if method == "POST" {
        head.push_str(&format!(
            "Content-Type: application/octet-stream\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
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
