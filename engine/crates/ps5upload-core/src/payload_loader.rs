//! Send the ps5upload.elf payload over the PS5 ELF loader.
//!
//! The PS5-side loader convention is a single-shot TCP port (9021 by
//! default) that reads the raw ELF bytes and jumps to entry. Shared
//! convention across every common PS5 homebrew loader, so "send to
//! :9021" works regardless of which loader is running on the console.
//!
//! This is what the Connection-screen "Send Payload" button ultimately
//! invokes. We keep it in `ps5upload-core` (not a dedicated crate)
//! because the protocol is three lines: connect, write bytes, close.
//!
//! No Unix-socket IPC here. Any PS5-side IPC channels live inside the
//! PS5 sandbox — only reachable from code running on the console.
//! Host-side integration lives entirely on TCP :9021 (ELF load) and
//! optionally HTTP :12800 (DPI PKG installer, not wired here yet).

use anyhow::{Context, Result};
use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

/// Default ELF-loader TCP port. Shared convention across every
/// common PS5 homebrew loader.
pub const DEFAULT_LOADER_PORT: u16 = 9021;

/// Connect timeout — the loader binds eagerly when it's running and drops
/// TCP SYNs when it isn't, so 3s is plenty.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Send timeout — even a 150 MB payload over 100 Mbit/s LAN completes in
/// ~12 s. 60 s covers wifi + arm64 loaders that may relink slowly.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Send a payload ELF file to the PS5's ELF loader.
///
/// Opens one TCP connection to `host:port`, writes the full contents of
/// `elf_path`, shuts down the write half to signal EOF, and closes. The
/// loader reads to EOF, maps, and jumps to entry — there is no
/// application-level framing or acknowledgement.
///
/// On success the caller should wait ~2 seconds and then probe the
/// payload's runtime port (9113) to confirm the payload booted.
pub fn send_payload(host: &str, port: u16, elf_path: &Path) -> Result<SendPayloadResult> {
    let bytes = std::fs::read(elf_path)
        .with_context(|| format!("failed to read ELF at {}", elf_path.display()))?;

    // Basic ELF sanity check so we fail cleanly on wrong-file-selected
    // rather than dumping bytes into the PS5's loader and hanging.
    if bytes.len() < 4 || &bytes[..4] != b"\x7FELF" {
        anyhow::bail!(
            "not an ELF file: {} (first 4 bytes {:02x?})",
            elf_path.display(),
            &bytes[..bytes.len().min(4)]
        );
    }

    let addr = resolve_addr(host, port)?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).with_context(|| {
        format!("connect to {addr} failed (is the PS5 on and the loader listening?)")
    })?;
    stream
        .set_write_timeout(Some(SEND_TIMEOUT))
        .context("set_write_timeout failed")?;

    stream
        .write_all(&bytes)
        .context("failed while streaming ELF bytes")?;
    // Signal EOF to the loader without closing the read half first — this
    // mirrors what `nc -w 1` does, which is what `make send-payload` used
    // before this module existed.
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("shutdown write failed")?;

    Ok(SendPayloadResult {
        bytes_sent: bytes.len() as u64,
        host: host.to_string(),
        port,
    })
}

#[derive(Debug, Clone)]
pub struct SendPayloadResult {
    pub bytes_sent: u64,
    pub host: String,
    pub port: u16,
}

fn resolve_addr(host: &str, port: u16) -> Result<SocketAddr> {
    let mut iter = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("DNS resolution failed for {host}:{port}"))?;
    iter.next()
        .with_context(|| format!("no addresses for {host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    fn write_elf_fixture(dir: &Path, name: &str, extra_bytes: usize) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut buf = b"\x7FELF".to_vec();
        buf.extend(vec![0xabu8; extra_bytes]);
        std::fs::write(&p, &buf).unwrap();
        p
    }

    #[test]
    fn rejects_non_elf_file() {
        let tmp = tempdir();
        let p = tmp.join("fake.elf");
        std::fs::write(&p, b"NOT AN ELF").unwrap();

        let err = send_payload("127.0.0.1", 0, &p).unwrap_err();
        assert!(err.to_string().contains("not an ELF file"));
    }

    #[test]
    fn sends_bytes_to_loopback_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let received = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).unwrap();
            buf
        });

        let tmp = tempdir();
        let path = write_elf_fixture(&tmp, "ok.elf", 1024);

        let res = send_payload("127.0.0.1", port, &path).unwrap();
        assert_eq!(res.bytes_sent, 1028); // 4 magic + 1024 padding
        assert_eq!(res.port, port);

        let got = received.join().unwrap();
        assert_eq!(got.len(), 1028);
        assert_eq!(&got[..4], b"\x7FELF");
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ps5upload_payload_loader_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
