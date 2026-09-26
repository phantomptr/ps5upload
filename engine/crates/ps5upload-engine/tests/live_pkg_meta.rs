//! Mount a staged .pkg on the console and read its param.sfo, to determine
//! CATEGORY (gd=base app, gp=patch/update, ac=DLC) + ids — the discriminator
//! for "is this installable standalone or an update needing a base game".
//!
//!   PS5UPLOAD_LIVE_IP=192.168.86.100 \
//!   PS5UPLOAD_LIVE_PKGPATH=/user/data/ps5upload/pkg_temp/PPSA01650.pkg \
//!     cargo test -p ps5upload-engine --test live_pkg_meta -- --nocapture --ignored

use ps5upload_core::diagnostics::pkg_direct_mount;
use ps5upload_core::fs_ops::fs_read;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

/// Minimal param.sfo (PSF) string-field extractor → (key, value) pairs.
fn parse_sfo(buf: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if buf.len() < 20 || &buf[0..4] != b"\x00PSF" {
        return out;
    }
    let rd32 = |o: usize| -> usize {
        u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]) as usize
    };
    let key_tbl = rd32(0x08);
    let data_tbl = rd32(0x0C);
    let entries = rd32(0x10);
    if key_tbl >= buf.len() || data_tbl >= buf.len() || entries > 1024 {
        return out;
    }
    for i in 0..entries {
        let e = 0x14 + i * 16;
        if e + 16 > buf.len() {
            break;
        }
        let key_off = u16::from_le_bytes([buf[e], buf[e + 1]]) as usize;
        let data_len = rd32(e + 4);
        let data_off = rd32(e + 12);
        let kpos = key_tbl + key_off;
        if kpos >= buf.len() {
            continue;
        }
        let kend = buf[kpos..].iter().position(|&b| b == 0).unwrap_or(0) + kpos;
        let key = String::from_utf8_lossy(&buf[kpos..kend]).to_string();
        let dpos = data_tbl + data_off;
        if dpos + data_len > buf.len() {
            continue;
        }
        let raw = &buf[dpos..dpos + data_len];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let val = String::from_utf8_lossy(&raw[..end]).to_string();
        out.push((key, val));
    }
    out
}

#[test]
#[ignore = "requires a live PS5 + a staged .pkg path (PS5UPLOAD_LIVE_IP / PS5UPLOAD_LIVE_PKGPATH)"]
fn live_inspect_pkg_category() {
    let ip = match env("PS5UPLOAD_LIVE_IP") {
        Some(v) => v,
        None => return,
    };
    let pkg_path = env("PS5UPLOAD_LIVE_PKGPATH")
        .expect("set PS5UPLOAD_LIVE_PKGPATH to the staged .pkg path on the console");
    let mgmt = format!("{ip}:9114");

    println!("mounting {pkg_path} ...");
    let m = pkg_direct_mount(&mgmt, &pkg_path, None).expect("pkg_direct_mount");
    let mount = m.mount_point.clone().unwrap_or_default();
    println!(
        "  ok={} code={:?} mount_point={:?} err={:?}",
        m.ok, m.code, m.mount_point, m.err
    );
    assert!(m.ok && !mount.is_empty(), "mount failed");

    let sfo_path = format!("{mount}/sce_sys/param.sfo");
    println!("reading {sfo_path} ...");
    let buf = fs_read(&mgmt, &sfo_path, 0, 256 * 1024).expect("fs_read param.sfo");
    println!("  read {} bytes", buf.len());

    let fields = parse_sfo(&buf);
    assert!(!fields.is_empty(), "param.sfo had no parseable fields");
    println!("---- param.sfo ----");
    for (k, v) in &fields {
        if matches!(
            k.as_str(),
            "CATEGORY" | "TITLE_ID" | "CONTENT_ID" | "TITLE" | "APP_VER" | "VERSION" | "APP_TYPE"
        ) {
            println!("  {k:20} = {v}");
        }
    }
    let category = fields
        .iter()
        .find(|(k, _)| k == "CATEGORY")
        .map(|(_, v)| v.as_str());
    println!(
        "\n==> CATEGORY = {category:?}  ({})",
        match category {
            Some("gd") | Some("gde") => "BASE APP — installable standalone",
            Some("gp") | Some("gpc") | Some("gpd") =>
                "PATCH/UPDATE — needs the base game installed first",
            Some("ac") => "DLC — needs the base game installed first",
            _ => "unknown",
        }
    );
}
