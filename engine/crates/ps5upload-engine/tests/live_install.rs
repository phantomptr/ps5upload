//! Live end-to-end .pkg install against a real PS5, exercising the new
//! launchability verification. Gated on env so it never runs in CI:
//!
//!   PS5UPLOAD_LIVE_IP=192.168.86.100 \
//!   PS5UPLOAD_LIVE_PKG=/path/to/file.pkg \
//!     cargo test -p ps5upload-engine --test live_install -- --nocapture --ignored
//!
//! Flow (the production path): parse header → stage to the console over the
//! transfer port → pkg_install with the bare local path (Tier 0/1) → poll
//! status → verify_launchable until the title registers under /user/app.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ps5upload_core::pkg_install::{
    pkg_install, pkg_install_status, verify_title_registered, via_tier, InstallPhase, LaunchCheck,
    PkgInstallRequest,
};
use ps5upload_core::transfer::{transfer_file_path, TransferConfig};
use ps5upload_pkg::parse_pkg;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

/// Minimal Range-capable static HTTP server so the PS5's BGFT can fetch the
/// .pkg directly (the InstallByPackage-over-HTTP path, which the cascade
/// treats as launchable Tier 1 and which skips the local appinst-local tier).
/// Serves the one file for every request path. Leaked thread — lives for the
/// test process; fine for a one-shot harness.
fn spawn_file_server(port: u16, file: PathBuf) {
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind http host");
    let data = Arc::new(std::fs::read(&file).expect("read pkg into memory"));
    thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let data = Arc::clone(&data);
            thread::spawn(move || {
                let _ = serve_one(&mut s, &data);
            });
        }
    });
}

fn serve_one(s: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 32768 {
            break;
        }
    }
    let req = String::from_utf8_lossy(&buf);
    let is_head = req.starts_with("HEAD");
    let total = data.len();
    eprintln!(
        "[http] {} {}",
        req.lines().next().unwrap_or("?"),
        req.lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .unwrap_or("(no range)")
    );
    let range = req.lines().find_map(|l| {
        if l.to_ascii_lowercase().starts_with("range:") {
            let v = l.split('=').nth(1)?.trim();
            let mut it = v.split('-');
            let start: usize = it.next()?.trim().parse().ok()?;
            let end = it
                .next()
                .and_then(|e| e.trim().parse::<usize>().ok())
                .unwrap_or(total - 1);
            Some((start, end.min(total - 1)))
        } else {
            None
        }
    });
    match range {
        Some((start, end)) if start <= end && start < total => {
            let body = &data[start..=end];
            let hdr = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(hdr.as_bytes())?;
            if !is_head {
                s.write_all(body)?;
            }
        }
        _ => {
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
            );
            s.write_all(hdr.as_bytes())?;
            if !is_head {
                s.write_all(data)?;
            }
        }
    }
    s.flush()
}

/// Recover a PPSA/CUSA title_id from a filename like
/// `PS5_PPSA01650_v1.03.pkg` — a fallback for packages whose embedded metadata
/// is absent, encrypted, or otherwise unreadable.
fn title_id_from_filename(name: &str) -> Option<String> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|tok| {
            tok.len() == 9
                && tok.as_bytes()[..4].iter().all(u8::is_ascii_uppercase)
                && tok.as_bytes()[4..].iter().all(u8::is_ascii_digit)
        })
        .map(|s| s.to_string())
}

#[test]
#[ignore = "requires a live PS5 + a real .pkg (set PS5UPLOAD_LIVE_IP / PS5UPLOAD_LIVE_PKG)"]
fn live_install_and_verify() {
    let ip = match env("PS5UPLOAD_LIVE_IP") {
        Some(v) => v,
        None => {
            eprintln!("PS5UPLOAD_LIVE_IP unset — skipping live install");
            return;
        }
    };
    let pkg = match env("PS5UPLOAD_LIVE_PKG") {
        Some(v) => v,
        None => {
            eprintln!("PS5UPLOAD_LIVE_PKG unset — skipping live install");
            return;
        }
    };
    let mgmt_addr = format!("{ip}:9114");
    let transfer_addr = format!("{ip}:9113");

    // 1. Parse the header. PS5 \x7FFIH packages normally expose metadata via
    //    their embedded CNT, but keep the filename fallback for encrypted or
    //    unusual packages whose content_id remains unreadable.
    let meta = parse_pkg(Path::new(&pkg)).expect("parse_pkg");
    let fname = Path::new(&pkg)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let title_id = if !meta.title_id.is_empty() {
        meta.title_id.clone()
    } else {
        title_id_from_filename(&fname).expect("could not derive title_id from header or filename")
    };
    println!("---- parsed pkg ----");
    println!("  filename     = {fname}");
    println!("  content_id   = {:?}", meta.content_id);
    println!("  title_id     = {title_id}  (header={:?})", meta.title_id);
    println!("  size         = {} bytes", meta.size);
    println!("  package_type = {:?}", meta.package_type);

    // 2. Decide the install URL. Two modes:
    //    - HTTP (PS5UPLOAD_LIVE_HTTP_HOST=<my-lan-ip:port>): serve the .pkg
    //      ourselves and hand BGFT an http:// URL. The cascade SKIPS the
    //      local appinst-local tier and uses InstallByPackage (the launchable
    //      Tier 1). No console staging needed.
    //    - Local (default): stage to /user/data and install the bare path.
    let url = if let Some(http_host) = env("PS5UPLOAD_LIVE_HTTP_HOST") {
        let port: u16 = http_host
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("PS5UPLOAD_LIVE_HTTP_HOST must be host:port");
        spawn_file_server(port, PathBuf::from(&pkg));
        let url = format!("http://{http_host}/{title_id}.pkg");
        println!("\n---- HTTP mode: BGFT will fetch {url} ----");
        url
    } else {
        let dest = format!("/user/data/ps5upload/pkg_temp/{title_id}.pkg");
        println!("\n---- staging to {dest} ----");
        let cfg = TransferConfig::new(transfer_addr.clone());
        let tx_id: [u8; 16] = [
            0x05, 0x05, 0x10, 0xad, 0x00, 0x01, 0x65, 0x00, 0xde, 0xad, 0xbe, 0xef, 0x00, 0x00,
            0x00, 0x01,
        ];
        let t0 = Instant::now();
        let r =
            transfer_file_path(&cfg, tx_id, &dest, Path::new(&pkg)).expect("transfer_file_path");
        println!(
            "  staged {} bytes in {} shards ({:.1}s)",
            r.bytes_sent,
            r.shards_sent,
            t0.elapsed().as_secs_f64()
        );
        dest
    };

    // 3. Kick the install.
    let req = PkgInstallRequest {
        url: url.clone(),
        content_id: meta.content_id.clone(),
        size: meta.size,
        title: if meta.title.is_empty() {
            title_id.clone()
        } else {
            meta.title.clone()
        },
        package_type: meta
            .package_type
            .clone()
            .unwrap_or_else(|| "PS4GD".to_string()),
        method: std::env::var("PS5UPLOAD_METHOD")
            .ok()
            .filter(|s| !s.is_empty()),
    };
    println!("\n---- pkg_install ----");
    let resp = pkg_install(&mgmt_addr, &req).expect("pkg_install RPC");
    eprintln!(
        "[install] task_id=0x{:08X} err_code=0x{:08X} register_path={} via={} intdebug_avail={} kernel_rw={} shellui_err={:?} appinst_err={:?} detail={:?}",
        resp.task_id, resp.err_code, resp.register_path, via_tier(resp.task_id),
        resp.intdebug_avail, resp.kernel_rw, resp.shellui_err, resp.appinst_err, resp.detail,
    );
    assert_eq!(
        resp.err_code, 0,
        "install register rejected: 0x{:08X} ({})",
        resp.err_code, resp.register_path
    );

    // 4. Poll status to terminal (the synthetic-DONE tiers report Done fast).
    println!("\n---- polling status ----");
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut phase = InstallPhase::Queued;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        match pkg_install_status(&mgmt_addr, resp.task_id) {
            Ok(s) => {
                println!(
                    "  phase={:?} {}/{} err=0x{:08X}",
                    s.phase, s.downloaded, s.total, s.err_code
                );
                phase = s.phase;
                if matches!(phase, InstallPhase::Done | InstallPhase::Error) {
                    break;
                }
            }
            Err(e) => {
                println!("  status poll error (continuing): {e}");
            }
        }
    }
    assert!(
        matches!(phase, InstallPhase::Done),
        "install did not reach Done (last phase {phase:?})"
    );

    // 5. The payoff: verify the title actually became launchable. This is the
    //    elf-arsenal wait_for_install_row step, via the /user/app scan.
    println!("\n---- verifying launchability ----");
    let vdeadline = Instant::now() + Duration::from_secs(180);
    let verdict = loop {
        let v = verify_title_registered(&mgmt_addr, &title_id);
        println!("  verify_title_registered({title_id}) = {v:?}");
        if v == LaunchCheck::Registered || Instant::now() >= vdeadline {
            break v;
        }
        std::thread::sleep(Duration::from_secs(3));
    };

    assert_eq!(
        verdict,
        LaunchCheck::Registered,
        "title {title_id} never registered as launchable within the window"
    );
    println!("\n✅ INSTALLED + VERIFIED LAUNCHABLE: {title_id}");
}
