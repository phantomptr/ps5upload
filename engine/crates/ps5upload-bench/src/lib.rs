//! Shared mock server used across all ps5upload benchmarks.
//!
//! The bench server is deliberately minimal — it performs no disk I/O and
//! does the absolute minimum to produce valid FTX2 ACK frames.  This means
//! the benchmarks measure host-side throughput (BLAKE3 + TCP + serialization)
//! without any PS5 storage bottleneck, which is what we care about.

use ftx2_proto::{
    AckState, FrameHeader, FrameType, ShardAck, ShardHeader, FRAME_HEADER_LEN, SHARD_HEADER_LEN,
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

// ─── I/O helpers ──────────────────────────────────────────────────────────────

pub fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> bool {
    let mut n = 0;
    while n < buf.len() {
        match s.read(&mut buf[n..]) {
            Ok(0) => return false,
            Ok(k) => n += k,
            Err(_) => return false,
        }
    }
    true
}

pub fn drain(s: &mut TcpStream, mut remaining: u64) {
    let mut tmp = [0u8; 65536];
    while remaining > 0 {
        let want = remaining.min(tmp.len() as u64) as usize;
        match s.read(&mut tmp[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => remaining -= n as u64,
        }
    }
}

pub fn send_frame(s: &mut TcpStream, ft: FrameType, body: &[u8]) {
    let hdr = FrameHeader::new(ft, 0, body.len() as u64, 0).encode();
    let _ = s.write_all(&hdr);
    let _ = s.write_all(body);
}

// ─── Connection handler ───────────────────────────────────────────────────────

fn handle_conn(mut s: TcpStream) {
    // Loop until EOF: production transfers send Hello → BeginTx → N × StreamShard
    // → CommitTx on a single persistent TCP connection. Returning after one
    // frame would leave the client reading from a closed socket and the bench
    // would crash with "failed to fill whole buffer" on the second frame.
    let mut hdr_buf = [0u8; FRAME_HEADER_LEN];
    loop {
        if !read_exact(&mut s, &mut hdr_buf) {
            return;
        }
        let hdr = match FrameHeader::decode(&hdr_buf) {
            Ok(h) => h,
            Err(_) => return,
        };
        let ft = match hdr.frame_type() {
            Ok(f) => f,
            Err(_) => return,
        };

        match ft {
            FrameType::Hello => {
                drain(&mut s, hdr.body_len);
                send_frame(&mut s, FrameType::HelloAck, br#"{"version":1}"#);
            }
            FrameType::Status => {
                drain(&mut s, hdr.body_len);
                send_frame(
                    &mut s,
                    FrameType::StatusAck,
                    br#"{"active_transactions":0}"#,
                );
            }
            FrameType::BeginTx => {
                drain(&mut s, hdr.body_len);
                send_frame(&mut s, FrameType::BeginTxAck, br#"{"accepted":true}"#);
            }
            FrameType::StreamShard => {
                if hdr.body_len < SHARD_HEADER_LEN as u64 {
                    send_frame(&mut s, FrameType::Error, b"shard_header_too_short");
                    return;
                }
                let mut shdr_buf = [0u8; SHARD_HEADER_LEN];
                if !read_exact(&mut s, &mut shdr_buf) {
                    return;
                }
                let shdr = match ShardHeader::decode(&shdr_buf) {
                    Ok(h) => h,
                    Err(_) => {
                        send_frame(&mut s, FrameType::Error, b"bad_shard_header");
                        return;
                    }
                };
                // Drain shard data without storing it (benchmark measures host side).
                drain(&mut s, hdr.body_len - SHARD_HEADER_LEN as u64);

                let ack = ShardAck {
                    tx_id: shdr.tx_id,
                    shard_seq: shdr.shard_seq,
                    ack_state: AckState::Spooled,
                    bytes_committed_total: 0,
                    files_committed_total: 0,
                };
                send_frame(&mut s, FrameType::ShardAck, &ack.encode());
            }
            FrameType::CommitTx => {
                drain(&mut s, hdr.body_len);
                send_frame(&mut s, FrameType::CommitTxAck, br#"{"committed":true}"#);
            }
            FrameType::AbortTx => {
                drain(&mut s, hdr.body_len);
                send_frame(&mut s, FrameType::AbortTxAck, b"{}");
            }
            _ => {
                drain(&mut s, hdr.body_len);
                send_frame(&mut s, FrameType::Error, b"unsupported");
            }
        }
    }
}

// ─── Server ───────────────────────────────────────────────────────────────────

/// Start a minimal in-process FTX2 mock server and return its address.
///
/// The server runs in a background thread pool and lives as long as the
/// process.  It is intentionally not shut down between benchmark iterations
/// — each criterion run gets the same long-lived server.
pub fn start_bench_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind bench server");
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || handle_conn(stream));
        }
    });
    addr
}
