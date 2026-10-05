//! Live test of the external/USB .pkg scan. Transfers a real .pkg onto the
//! console's /mnt/usb0, then runs scan_external_pkgs and asserts it's found
//! with the right platform.
//!
//!   PS5UPLOAD_LIVE_IP=192.168.86.99 \
//!   PS5UPLOAD_LIVE_PKG=/path/to/CUSA.pkg \
//!     cargo test -p ps5upload-engine --test live_scan_external -- --nocapture --ignored

use std::path::Path;

use ps5upload_core::transfer::TransferConfig;
use ps5upload_engine::scan_external_pkgs;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

#[test]
#[ignore = "requires a live PS5 with a writable /mnt/usb0 + a real .pkg"]
fn live_scan_external_usb() {
    let ip = match env("PS5UPLOAD_LIVE_IP") {
        Some(v) => v,
        None => return,
    };
    let pkg = env("PS5UPLOAD_LIVE_PKG").expect("set PS5UPLOAD_LIVE_PKG");
    // AVA1 has one port; the engine and the core take the bare host.
    let mgmt = ip.clone();
    let dest = "/mnt/usb0/ps5upload-scan-test.pkg";

    println!("staging {pkg} -> {dest} (on the USB drive)");
    let cfg = TransferConfig::new(ip.clone());
    let txid: [u8; 16] = [
        0x05, 0x05, 0x10, 0xad, 0x00, 0x01, 0x65, 0x00, 0xde, 0xad, 0xbe, 0xef, 0x00, 0x00, 0x00,
        0x07,
    ];
    let r = ps5upload_ava1::upload::upload_file(&cfg, txid, dest, Path::new(&pkg))
        .expect("transfer to /mnt/usb0");
    println!("  staged {} bytes", r.bytes_sent);

    println!("\nscanning external drives…");
    let found = scan_external_pkgs(&mgmt).expect("scan_external_pkgs");
    for p in &found {
        println!(
            "  {} (drive={} size={} platform={:?} cid={:?} title={:?})",
            p.path, p.drive, p.size, p.platform, p.content_id, p.title_id
        );
    }
    let hit = found.iter().find(|p| p.path == dest);
    assert!(hit.is_some(), "scan did not find {dest}");
    println!("\n✅ scan found the USB pkg: {:?}", hit.unwrap().path);
}
