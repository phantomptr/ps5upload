//! ps5upload-lab — CLI tool for exercising the payload2 control channel.
//!
//! Usage:
//!   ps5upload-lab [ADDR] COMMAND [ARGS...]
//!
//! Default ADDR: 192.168.137.2:9113
//!
//! Commands:
//!   hello                           send HELLO, print HELLO_ACK
//!   status                          send STATUS, print STATUS_ACK body
//!   shutdown                        send SHUTDOWN
//!   takeover                        send TAKEOVER_REQUEST
//!   begin-tx TX_ID_HEX              send BEGIN_TX with the given 32-hex-char tx_id
//!   query-tx TX_ID_HEX              send QUERY_TX for the given tx_id
//!   abort-tx TX_ID_HEX              send ABORT_TX for the given tx_id
//!   send-shard TX_ID_HEX SEQ        send a dummy STREAM_SHARD and print SHARD_ACK
//!   transfer TX_ID_HEX DEST FILE    single-file transfer (begin→shards→commit→query)
//!   transfer-dir TX_ID_HEX DEST DIR multi-file transfer of a local directory
//!   volumes                         enumerate PS5 storage volumes (FS_LIST_VOLUMES)

use anyhow::{bail, Context, Result};
use ftx2_proto::{FrameType, ShardAck, ShardHeader, TxMeta};
use ps5upload_core::connection::Connection;
use ps5upload_core::fs_ops::{app_launch, app_list_registered, app_register, app_unregister};
use ps5upload_core::hash_shard;
use ps5upload_core::transfer::{transfer_dir, transfer_file, TransferConfig};
use ps5upload_core::volumes::list_volumes;

const DEFAULT_ADDR: &str = "192.168.137.2:9113";

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_tx_id(hex: &str) -> Result<[u8; 16]> {
    if hex.len() != 32 {
        bail!("tx_id must be exactly 32 hex chars, got {}", hex.len());
    }
    let mut out = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        b'A'..=b'F' => Ok(10 + b - b'A'),
        _ => bail!("invalid hex char: {}", b as char),
    }
}

fn tx_meta_body(tx_id: [u8; 16], kind: u32, extra: &[u8]) -> Vec<u8> {
    let meta = TxMeta {
        tx_id,
        kind,
        flags: 0,
    };
    let mut buf = meta.encode().to_vec();
    buf.extend_from_slice(extra);
    buf
}

fn expect_frame(conn: &mut Connection, expected: FrameType) -> Result<Vec<u8>> {
    let (hdr, body) = conn.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    println!("frame_type={ft:?}");
    if ft != expected {
        eprintln!("  body: {}", String::from_utf8_lossy(&body));
        bail!("expected {expected:?}, got {ft:?}");
    }
    Ok(body)
}

// ─── Commands ────────────────────────────────────────────────────────────────

fn do_volumes(addr: &str) -> Result<()> {
    let vols = list_volumes(addr)?;
    if vols.volumes.is_empty() {
        println!("(no volumes detected)");
        return Ok(());
    }
    // Render a small human-readable table. Fixed-width columns so the
    // common case (3-5 volumes, short mount names) is easy to eyeball.
    println!(
        "{:<8}  {:<8}  {:>14}  {:>14}  RW",
        "PATH", "FS", "TOTAL", "FREE"
    );
    for v in &vols.volumes {
        println!(
            "{:<8}  {:<8}  {:>14}  {:>14}  {}",
            v.path,
            v.fs_type,
            format_bytes(v.total_bytes),
            format_bytes(v.free_bytes),
            if v.writable { "rw" } else { "ro" }
        );
    }
    Ok(())
}

fn format_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.2} {}", v, UNITS[i])
}

fn do_hello(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::Hello, b"{}")?;
    let body = expect_frame(&mut c, FrameType::HelloAck)?;
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

fn do_status(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::Status, b"")?;
    let body = expect_frame(&mut c, FrameType::StatusAck)?;
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

fn do_shutdown(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::Shutdown, b"")?;
    let (hdr, body) = c.recv_frame()?;
    println!(
        "frame_type={:?}",
        hdr.frame_type().unwrap_or(FrameType::Error)
    );
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

fn do_takeover(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::TakeoverRequest, b"")?;
    let (hdr, body) = c.recv_frame()?;
    println!(
        "frame_type={:?}",
        hdr.frame_type().unwrap_or(FrameType::Error)
    );
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

fn do_begin_tx(addr: &str, tx_id_hex: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let extra = format!(r#"{{"tx_id":"{}"}}"#, tx_id_hex);
    let body = tx_meta_body(tx_id, 1 /* upload_tree */, extra.as_bytes());
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::BeginTx, &body)?;
    let resp = expect_frame(&mut c, FrameType::BeginTxAck)?;
    println!("{}", String::from_utf8_lossy(&resp));
    Ok(())
}

fn do_query_tx(addr: &str, tx_id_hex: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let body = tx_meta_body(tx_id, 0, b"");
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::QueryTx, &body)?;
    let resp = expect_frame(&mut c, FrameType::QueryTxAck)?;
    println!("{}", String::from_utf8_lossy(&resp));
    Ok(())
}

fn do_commit_tx(addr: &str, tx_id_hex: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let body = tx_meta_body(tx_id, 0, b"");
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::CommitTx, &body)?;
    let resp = expect_frame(&mut c, FrameType::CommitTxAck)?;
    println!("{}", String::from_utf8_lossy(&resp));
    Ok(())
}

fn do_abort_tx(addr: &str, tx_id_hex: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let body = tx_meta_body(tx_id, 0, b"");
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AbortTx, &body)?;
    let resp = expect_frame(&mut c, FrameType::AbortTxAck)?;
    println!("{}", String::from_utf8_lossy(&resp));
    Ok(())
}

fn do_transfer(addr: &str, tx_id_hex: &str, dest: &str, file_path: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let data = std::fs::read(file_path).with_context(|| format!("read {file_path}"))?;
    let cfg = TransferConfig::new(addr);
    println!(
        "transfer: file={file_path} bytes={} dest={dest}",
        data.len()
    );
    let r = transfer_file(&cfg, tx_id, dest, &data)?;
    println!(
        "done: shards={} bytes={} tx={}",
        r.shards_sent, r.bytes_sent, r.tx_id_hex
    );
    println!("commit_ack: {}", r.commit_ack_body);
    do_query_tx(addr, tx_id_hex)
}

fn do_transfer_dir(addr: &str, tx_id_hex: &str, dest_root: &str, src_dir: &str) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;
    let cfg = TransferConfig::new(addr);
    let r = transfer_dir(&cfg, tx_id, dest_root, std::path::Path::new(src_dir))?;
    println!(
        "done: shards={} bytes={} tx={}",
        r.shards_sent, r.bytes_sent, r.tx_id_hex
    );
    println!("commit_ack: {}", r.commit_ack_body);
    do_query_tx(addr, tx_id_hex)
}

fn do_send_shard(addr: &str, tx_id_hex: &str, shard_seq: u64) -> Result<()> {
    let tx_id = parse_tx_id(tx_id_hex)?;

    // Dummy shard: 256 bytes of 0xAB payload.
    let shard_data = vec![0xABu8; 256];
    let shard_hdr = ShardHeader {
        tx_id,
        shard_seq,
        shard_digest: hash_shard(&shard_data),
        record_count: 1,
        flags: 0,
    };
    let hdr_bytes = shard_hdr.encode();

    let mut c = Connection::connect(addr)?;
    c.send_frame_split(FrameType::StreamShard, &hdr_bytes, &shard_data)?;

    // Receive SHARD_ACK (binary body)
    let (resp_hdr, resp_body) = c.recv_frame()?;
    let ft = resp_hdr.frame_type().unwrap_or(FrameType::Error);
    println!("frame_type={ft:?}");

    if ft == FrameType::ShardAck {
        match ShardAck::decode(&resp_body) {
            Ok(ack) => {
                println!(
                    "shard_seq={} ack_state={:?} bytes_committed={} files_committed={}",
                    ack.shard_seq,
                    ack.ack_state,
                    ack.bytes_committed_total,
                    ack.files_committed_total,
                );
            }
            Err(e) => eprintln!("failed to decode SHARD_ACK: {e}"),
        }
    } else {
        eprintln!("body: {}", String::from_utf8_lossy(&resp_body));
        bail!("expected SHARD_ACK, got {ft:?}");
    }
    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn usage() -> ! {
    eprintln!("Usage: ps5upload-lab [ADDR] COMMAND [ARGS...]");
    eprintln!("  Default ADDR: {DEFAULT_ADDR}");
    eprintln!("Commands:");
    eprintln!("  hello");
    eprintln!("  status");
    eprintln!("  shutdown");
    eprintln!("  takeover");
    eprintln!("  begin-tx TX_ID_HEX");
    eprintln!("  query-tx TX_ID_HEX");
    eprintln!("  commit-tx TX_ID_HEX");
    eprintln!("  abort-tx  TX_ID_HEX");
    eprintln!("  send-shard   TX_ID_HEX SHARD_SEQ");
    eprintln!("  transfer     TX_ID_HEX DEST_FILE FILE_PATH");
    eprintln!("  transfer-dir TX_ID_HEX DEST_ROOT SRC_DIR");
    eprintln!("  register     SRC_PATH      register a game folder");
    eprintln!("  unregister   TITLE_ID      reverse registration");
    eprintln!("  launch       TITLE_ID      sceLncUtilLaunchApp");
    eprintln!("  apps                       list titles present in app.db");
    std::process::exit(1);
}

fn do_register(addr: &str, src_path: &str) -> Result<()> {
    /* lab CLI defaults to NOT patching DRM-type — for ad-hoc
     * registration of well-formed dumps. The desktop client UI
     * exposes the toggle. */
    let res = app_register(addr, src_path, false)?;
    println!(
        "registered: title_id={} title_name={} used_nullfs={}",
        res.title_id, res.title_name, res.used_nullfs
    );
    Ok(())
}

fn do_unregister(addr: &str, title_id: &str) -> Result<()> {
    app_unregister(addr, title_id)?;
    println!("unregistered: {title_id}");
    Ok(())
}

fn do_launch(addr: &str, title_id: &str) -> Result<()> {
    app_launch(addr, title_id)?;
    println!("launched: {title_id}");
    Ok(())
}

fn do_apps(addr: &str) -> Result<()> {
    let apps = app_list_registered(addr)?;
    if apps.apps.is_empty() {
        println!("(no registered titles)");
        return Ok(());
    }
    println!("{:<12} {:<40} {:<30} IMG", "TITLE_ID", "TITLE_NAME", "SRC");
    for a in &apps.apps {
        println!(
            "{:<12} {:<40.40} {:<30.30} {}",
            a.title_id,
            a.title_name,
            if a.src.is_empty() { "-" } else { &a.src },
            if a.image_backed { "yes" } else { "no" }
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    // If first arg looks like host:port or a bare IP, use it as address.
    let (addr, rest) =
        if args[0].contains(':') || args[0].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            (args[0].as_str(), &args[1..])
        } else {
            (DEFAULT_ADDR, &args[..])
        };

    if rest.is_empty() {
        usage();
    }

    match rest[0].as_str() {
        "hello" => do_hello(addr),
        "status" => do_status(addr),
        "volumes" => do_volumes(addr),
        "shutdown" => do_shutdown(addr),
        "takeover" => do_takeover(addr),
        "begin-tx" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_begin_tx(addr, tx_id)
        }
        "query-tx" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_query_tx(addr, tx_id)
        }
        "commit-tx" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_commit_tx(addr, tx_id)
        }
        "abort-tx" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_abort_tx(addr, tx_id)
        }
        "send-shard" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let seq: u64 = rest.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            do_send_shard(addr, tx_id, seq)
        }
        "transfer" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest_root = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let file_path = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer(addr, tx_id, dest_root, file_path)
        }
        "transfer-dir" => {
            let tx_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest_root = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let src_dir = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer_dir(addr, tx_id, dest_root, src_dir)
        }
        "register" => {
            let src_path = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_register(addr, src_path)
        }
        "unregister" => {
            let title_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_unregister(addr, title_id)
        }
        "launch" => {
            let title_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_launch(addr, title_id)
        }
        "apps" => do_apps(addr),
        cmd => bail!("unknown command: {cmd}"),
    }
}
