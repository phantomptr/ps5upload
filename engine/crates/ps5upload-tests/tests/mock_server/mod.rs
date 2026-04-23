//! Mock FTX2 server for integration tests.
//!
//! Runs on a random loopback port in the same process, accepts connections,
//! and processes frames on each connection in a loop until EOF (matching the
//! payload's persistent-connection behaviour). Implements the minimal subset
//! of the FTX2 protocol needed to exercise `ps5upload-core::transfer`.
//!
//! State is tracked in an `Arc<Mutex<MockState>>` shared between the listener
//! thread and the test.

use ftx2_proto::{
    AckState, FrameHeader, FrameType, ShardAck, ShardHeader, TxMeta, PACKED_RECORD_PREFIX_LEN,
    SHARD_FLAG_PACKED, TX_FLAG_RESUME,
};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

// ─── Mock state ───────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub(crate) struct MockTx {
    pub(crate) state: String,
    pub(crate) shards_received: u64,
    pub(crate) bytes_received: u64,
    pub(crate) dest_root: String,
    pub(crate) total_shards: u64,
    /// Spool: shard_seq → data bytes
    pub(crate) spool: HashMap<u64, Vec<u8>>,
}

#[derive(Debug)]
pub(crate) struct MockState {
    pub(crate) txs: HashMap<String, MockTx>, // tx_id_hex → MockTx
    pub(crate) commands: u64,
    /// Applied files: dest_path → data
    pub(crate) applied: HashMap<String, Vec<u8>>,
    /// JSON body to return for FS_LIST_VOLUMES. Tests can override this via
    /// `srv.state().lock().unwrap().volumes_json = ...` to simulate hot-plug
    /// or unusual configurations.
    pub(crate) volumes_json: String,
    /// Drop the TCP connection mid-stream after this many STREAM_SHARD
    /// frames have been ACKed. `None` = never drop. Used by resume
    /// tests to simulate a network interruption. The server marks the
    /// affected tx as "interrupted" so a subsequent BeginTx with
    /// TX_FLAG_RESUME can pick up where it left off.
    pub(crate) drop_after_shards: Option<u64>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            txs: HashMap::new(),
            commands: 0,
            applied: HashMap::new(),
            volumes_json: DEFAULT_VOLUMES_JSON.to_string(),
            drop_after_shards: None,
        }
    }
}

const DEFAULT_VOLUMES_JSON: &str = r#"{"volumes":[
{"path":"/data","fs_type":"ufs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true},
{"path":"/ext0","fs_type":"ufs","total_bytes":1000000000000,"free_bytes":900000000000,"writable":true}
]}"#;

// ─── Mock server ──────────────────────────────────────────────────────────────

pub(crate) struct MockServer {
    pub(crate) addr: String,
    // Tests reach in via `srv.state.lock()` to inspect what landed on the
    // mock disk or to arm per-test hooks (drop_after_shards, etc.). The
    // field is read from test files but not from mock_server.rs itself,
    // which trips `dead_code` since each integration test compiles as
    // its own crate and sees mock_server as "module with unused state."
    #[allow(dead_code)]
    pub(crate) state: Arc<Mutex<MockState>>,
}

impl MockServer {
    /// Start a mock FTX2 server on a random loopback port.
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state: Arc<Mutex<MockState>> = Arc::new(Mutex::new(MockState::default()));
        let state_clone = Arc::clone(&state);

        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let st = Arc::clone(&state_clone);
                        thread::spawn(move || handle_connection(s, st));
                    }
                    Err(_) => break,
                }
            }
        });

        MockServer { addr, state }
    }
}

// ─── Frame I/O helpers ────────────────────────────────────────────────────────

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> bool {
    let mut got = 0;
    while got < buf.len() {
        match stream.read(&mut buf[got..]) {
            Ok(0) => return false,
            Ok(n) => got += n,
            Err(_) => return false,
        }
    }
    true
}

fn send_frame(stream: &mut TcpStream, ft: FrameType, body: &[u8]) {
    let hdr = FrameHeader::new(ft, 0, body.len() as u64, 0).encode();
    stream.write_all(&hdr).ok();
    stream.write_all(body).ok();
}

fn send_error(stream: &mut TcpStream, msg: &str) {
    send_frame(stream, FrameType::Error, msg.as_bytes());
}

fn tx_id_hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

// ─── Connection handler ───────────────────────────────────────────────────────

fn handle_connection(stream: TcpStream, state: Arc<Mutex<MockState>>) {
    let state_clone = Arc::clone(&state);
    handle_connection_inner(stream, state);
    // When the connection drops mid-transfer, mark any still-active
    // transactions as "interrupted" — matches the payload's takeover
    // behavior and lets a subsequent BeginTx with TX_FLAG_RESUME find
    // and adopt the entry.
    let mut st = state_clone.lock().unwrap();
    for tx in st.txs.values_mut() {
        if tx.state == "active" {
            tx.state = "interrupted".to_string();
        }
    }
}

fn handle_connection_inner(mut stream: TcpStream, state: Arc<Mutex<MockState>>) {
    stream.set_nodelay(true).ok();
    loop {
        let mut hdr_buf = [0u8; ftx2_proto::FRAME_HEADER_LEN];
        if !read_exact(&mut stream, &mut hdr_buf) {
            return; // EOF — client closed
        }
        let hdr = match FrameHeader::decode(&hdr_buf) {
            Ok(h) => h,
            Err(_) => {
                send_error(&mut stream, "bad_header");
                return;
            }
        };
        let ft = match hdr.frame_type() {
            Ok(f) => f,
            Err(_) => {
                send_error(&mut stream, "unknown_frame_type");
                return;
            }
        };

        state.lock().unwrap().commands += 1;

        match ft {
            // ── HELLO ──────────────────────────────────────────────────────────
            FrameType::Hello => {
                // drain body
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                send_frame(&mut stream, FrameType::HelloAck, br#"{"version":1}"#);
            }

            // ── STATUS ─────────────────────────────────────────────────────────
            FrameType::Status => {
                let st = state.lock().unwrap();
                let body = format!(r#"{{"active_transactions":{}}}"#, st.txs.len());
                send_frame(&mut stream, FrameType::StatusAck, body.as_bytes());
            }

            // ── BEGIN_TX ───────────────────────────────────────────────────────
            FrameType::BeginTx => {
                let mut body = vec![0u8; hdr.body_len as usize];
                if !read_exact(&mut stream, &mut body) {
                    return;
                }
                let meta = match TxMeta::decode(&body) {
                    Ok(m) => m,
                    Err(_) => {
                        send_error(&mut stream, "invalid_tx_meta");
                        return;
                    }
                };
                let hex = tx_id_hex(&meta.tx_id);
                let extra = TxMeta::extra(&body);
                let extra_str = String::from_utf8_lossy(extra);
                let dest_root = extract_json_str(&extra_str, "dest_root").unwrap_or_default();
                let total_shards = extract_json_u64(&extra_str, "total_shards").unwrap_or(0);

                // RESUME semantics: if the flag is set AND an interrupted tx
                // with the same tx_id exists, re-adopt it and report back
                // `last_acked_shard = shards_received`. Otherwise allocate
                // fresh. Matches the payload-side behavior from the 2.1
                // runtime.c hunks.
                let is_resume = (meta.flags & TX_FLAG_RESUME) != 0;
                let mut st = state.lock().unwrap();
                let last_acked = if is_resume {
                    match st.txs.get_mut(&hex) {
                        Some(tx) if tx.state == "interrupted" => {
                            tx.state = "active".to_string();
                            tx.shards_received
                        }
                        _ => {
                            // Flag set but nothing to resume — treat as fresh.
                            st.txs.insert(
                                hex.clone(),
                                MockTx {
                                    state: "active".to_string(),
                                    dest_root,
                                    total_shards,
                                    ..Default::default()
                                },
                            );
                            0
                        }
                    }
                } else {
                    st.txs.insert(
                        hex.clone(),
                        MockTx {
                            state: "active".to_string(),
                            dest_root,
                            total_shards,
                            ..Default::default()
                        },
                    );
                    0
                };
                let resp = format!(
                    r#"{{"accepted":true,"tx_id":"{hex}","last_acked_shard":{last_acked}}}"#
                );
                send_frame(&mut stream, FrameType::BeginTxAck, resp.as_bytes());
            }

            // ── STREAM_SHARD ───────────────────────────────────────────────────
            FrameType::StreamShard => {
                if hdr.body_len < ftx2_proto::SHARD_HEADER_LEN as u64 {
                    send_error(&mut stream, "shard_header_too_short");
                    return;
                }
                let mut shard_hdr_bytes = [0u8; ftx2_proto::SHARD_HEADER_LEN];
                if !read_exact(&mut stream, &mut shard_hdr_bytes) {
                    return;
                }
                let shdr = match ShardHeader::decode(&shard_hdr_bytes) {
                    Ok(h) => h,
                    Err(_) => {
                        send_error(&mut stream, "bad_shard_header");
                        return;
                    }
                };
                let data_len = hdr.body_len - ftx2_proto::SHARD_HEADER_LEN as u64;
                let mut data = vec![0u8; data_len as usize];
                if !read_exact(&mut stream, &mut data) {
                    return;
                }

                // Verify BLAKE3 (skip if all-zero)
                let all_zero = shdr.shard_digest.iter().all(|&b| b == 0);
                if !all_zero {
                    let actual = ps5upload_core::hash_shard(&data);
                    if actual != shdr.shard_digest {
                        send_error(&mut stream, "shard_digest_mismatch");
                        return;
                    }
                }

                let packed = (shdr.flags & SHARD_FLAG_PACKED) != 0;
                let hex = tx_id_hex(&shdr.tx_id);

                if packed {
                    // Parse inline records: u32 path_len, u32 data_len, path, data
                    let mut off = 0usize;
                    let mut parsed_records = Vec::<(String, Vec<u8>)>::new();
                    let mut parse_ok = true;
                    for _ in 0..shdr.record_count {
                        if data.len() - off < PACKED_RECORD_PREFIX_LEN {
                            parse_ok = false;
                            break;
                        }
                        let path_len =
                            u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
                        let rec_data_len =
                            u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
                        off += PACKED_RECORD_PREFIX_LEN;
                        if data.len() - off < path_len + rec_data_len {
                            parse_ok = false;
                            break;
                        }
                        let path = match std::str::from_utf8(&data[off..off + path_len]) {
                            Ok(s) => s.to_string(),
                            Err(_) => {
                                parse_ok = false;
                                break;
                            }
                        };
                        off += path_len;
                        let rec_data = data[off..off + rec_data_len].to_vec();
                        off += rec_data_len;
                        parsed_records.push((path, rec_data));
                    }
                    if !parse_ok || off != data.len() {
                        send_error(&mut stream, "packed_parse_failed");
                        return;
                    }
                    let mut st = state.lock().unwrap();
                    for (path, rec_data) in parsed_records {
                        st.applied.insert(path, rec_data);
                    }
                    if let Some(tx) = st.txs.get_mut(&hex) {
                        tx.shards_received += 1;
                        tx.bytes_received += data_len;
                        let mut ack_bytes = [0u8; ftx2_proto::SHARD_ACK_LEN];
                        let ack = ShardAck {
                            tx_id: shdr.tx_id,
                            shard_seq: shdr.shard_seq,
                            ack_state: AckState::Spooled,
                            bytes_committed_total: tx.bytes_received,
                            files_committed_total: 0,
                        };
                        ack_bytes.copy_from_slice(&ack.encode());
                        send_frame(&mut stream, FrameType::ShardAck, &ack_bytes);
                    } else {
                        send_error(&mut stream, "tx_not_found");
                    }
                } else {
                    let mut st = state.lock().unwrap();
                    if let Some(tx) = st.txs.get_mut(&hex) {
                        tx.spool.insert(shdr.shard_seq, data.clone());
                        tx.shards_received += 1;
                        tx.bytes_received += data_len;

                        let mut ack_bytes = [0u8; ftx2_proto::SHARD_ACK_LEN];
                        let ack = ShardAck {
                            tx_id: shdr.tx_id,
                            shard_seq: shdr.shard_seq,
                            ack_state: AckState::Spooled,
                            bytes_committed_total: tx.bytes_received,
                            files_committed_total: 0,
                        };
                        ack_bytes.copy_from_slice(&ack.encode());
                        let shards_so_far = tx.shards_received;
                        let drop_threshold = st.drop_after_shards;
                        drop(st); // release lock before socket writes
                        send_frame(&mut stream, FrameType::ShardAck, &ack_bytes);
                        if let Some(n) = drop_threshold {
                            if shards_so_far >= n {
                                // Simulate a mid-stream TCP drop. The outer
                                // `handle_connection` wrapper will mark the
                                // tx "interrupted" on its way out.
                                return;
                            }
                        }
                    } else {
                        send_error(&mut stream, "tx_not_found");
                    }
                }
            }

            // ── COMMIT_TX ──────────────────────────────────────────────────────
            FrameType::CommitTx => {
                let mut body = vec![0u8; hdr.body_len as usize];
                if !read_exact(&mut stream, &mut body) {
                    return;
                }
                let meta = match TxMeta::decode(&body) {
                    Ok(m) => m,
                    Err(_) => {
                        send_error(&mut stream, "invalid_tx_meta");
                        return;
                    }
                };
                let hex = tx_id_hex(&meta.tx_id);
                // Do all state work under a single lock, then drop before
                // any socket writes. Holding the mutex across write_all
                // risked poisoning on broken-pipe panics and stalled
                // concurrent connection threads unnecessarily.
                enum CommitOutcome {
                    NotFound,
                    Incomplete,
                    Ok { dest_root: String },
                }
                let outcome = {
                    let mut st = state.lock().unwrap();
                    match st.txs.get_mut(&hex) {
                        None => CommitOutcome::NotFound,
                        Some(tx) if tx.total_shards > 0 && tx.shards_received < tx.total_shards => {
                            CommitOutcome::Incomplete
                        }
                        Some(tx) => {
                            // Assemble non-packed shard data in order and
                            // insert under `dest_root`. Skip this for
                            // packed-only transfers: the per-record
                            // streaming path already inserted each
                            // packed record into `applied` keyed by its
                            // absolute dest_path, and an empty-spool
                            // unconditional insert here would overwrite
                            // (or create) `applied[dest_root]` with an
                            // empty byte vec — which silently corrupts
                            // any test asserting on packed output.
                            let dest = tx.dest_root.clone();
                            tx.state = "committed".to_string();
                            let dest_root = tx.dest_root.clone();
                            if !tx.spool.is_empty() {
                                let mut file_data = Vec::new();
                                let mut seq = 1u64;
                                while let Some(chunk) = tx.spool.get(&seq) {
                                    file_data.extend_from_slice(chunk);
                                    seq += 1;
                                }
                                st.applied.insert(dest, file_data);
                            }
                            CommitOutcome::Ok { dest_root }
                        }
                    }
                };
                match outcome {
                    CommitOutcome::NotFound => send_error(&mut stream, "tx_not_found"),
                    CommitOutcome::Incomplete => send_error(&mut stream, "shards_incomplete"),
                    CommitOutcome::Ok { dest_root } => {
                        let resp = format!(
                            r#"{{"committed":true,"tx_id":"{hex}","dest_root":"{dest_root}"}}"#
                        );
                        send_frame(&mut stream, FrameType::CommitTxAck, resp.as_bytes());
                    }
                }
            }

            // ── ABORT_TX ───────────────────────────────────────────────────────
            FrameType::AbortTx => {
                let mut body = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut body);
                if let Ok(meta) = TxMeta::decode(&body) {
                    let hex = tx_id_hex(&meta.tx_id);
                    let mut st = state.lock().unwrap();
                    if let Some(tx) = st.txs.get_mut(&hex) {
                        tx.state = "aborted".to_string();
                    }
                }
                send_frame(&mut stream, FrameType::AbortTxAck, b"{}");
            }

            // ── QUERY_TX ───────────────────────────────────────────────────────
            FrameType::QueryTx => {
                let mut body = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut body);
                if let Ok(meta) = TxMeta::decode(&body) {
                    let hex = tx_id_hex(&meta.tx_id);
                    let st = state.lock().unwrap();
                    if let Some(tx) = st.txs.get(&hex) {
                        let resp = format!(
                            r#"{{"tx_id":"{hex}","state":"{}","shards_received":{}}}"#,
                            tx.state, tx.shards_received
                        );
                        send_frame(&mut stream, FrameType::QueryTxAck, resp.as_bytes());
                        return;
                    }
                }
                send_error(&mut stream, "tx_not_found");
            }

            // ── FS_LIST_VOLUMES ────────────────────────────────────────────────
            // Returns a fixed fake volume list so host-side parsing + helper
            // logic can be exercised without real hardware. Tests can mutate
            // `MockState::fake_volumes` if they want to simulate hot-plug etc.
            FrameType::FsListVolumes => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                let body = state.lock().unwrap().volumes_json.clone();
                send_frame(&mut stream, FrameType::FsListVolumesAck, body.as_bytes());
            }

            // ── App lifecycle stubs ───────────────────────────────────────────
            // These are minimal "accept and echo a plausible response" stubs
            // so host-side helpers (app_register / app_launch / etc) can be
            // exercised without a PS5. The real behaviour lives in
            // payload/src/register.c — the mock only needs to round-trip the
            // FTX2 framing and return well-formed JSON.
            FrameType::AppRegister => {
                let mut body = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut body);
                let json = String::from_utf8_lossy(&body);
                let src_path = extract_json_str(&json, "src_path").unwrap_or_default();
                if src_path.is_empty() {
                    send_error(&mut stream, "register_src_path_missing");
                    continue;
                }
                // Derive a plausible title_id from the basename so the
                // mock mirrors real-world output shape. "PPSA" + last 5
                // characters of the basename. Good enough for host tests.
                let leaf = src_path.rsplit('/').next().unwrap_or("UNKNOWN");
                let title_id = format!(
                    "PPSA{:0>5}",
                    leaf.chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(5)
                        .collect::<String>()
                );
                let resp = format!(
                    "{{\"title_id\":\"{title_id}\",\"title_name\":\"{leaf}\",\"used_nullfs\":true}}"
                );
                send_frame(&mut stream, FrameType::AppRegisterAck, resp.as_bytes());
            }
            FrameType::AppUnregister => {
                let mut body = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut body);
                let title_id = extract_json_str(&String::from_utf8_lossy(&body), "title_id")
                    .unwrap_or_default();
                if title_id.is_empty() {
                    send_error(&mut stream, "unregister_title_id_missing");
                    continue;
                }
                send_frame(&mut stream, FrameType::AppUnregisterAck, b"");
            }
            FrameType::AppLaunch => {
                let mut body = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut body);
                let title_id = extract_json_str(&String::from_utf8_lossy(&body), "title_id")
                    .unwrap_or_default();
                if title_id.is_empty() {
                    send_error(&mut stream, "launch_title_id_missing");
                    continue;
                }
                send_frame(&mut stream, FrameType::AppLaunchAck, b"");
            }
            FrameType::AppListRegistered => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                // Two-entry fixture: one image-backed title and one native
                // folder. Exercises both branches of the Library UI's
                // registration display.
                let resp = "{\"apps\":[\
                    {\"title_id\":\"PPSA00123\",\"title_name\":\"Test Game\",\
                     \"src\":\"/data/homebrew/test\",\"image_backed\":false},\
                    {\"title_id\":\"CUSA00456\",\"title_name\":\"Backported\",\
                     \"src\":\"/mnt/ps5upload/bp/game\",\"image_backed\":true}]}";
                send_frame(
                    &mut stream,
                    FrameType::AppListRegisteredAck,
                    resp.as_bytes(),
                );
            }
            // ── Hardware monitoring stubs — GET_HW_INFO / GET_TEMPS /
            //    GET_POWER_INFO. Text-body wire shape: newline-separated
            //    key=value. Fixtures here mirror a real PS5's typical
            //    readings so the client-side parser + Hardware tab can
            //    be exercised end-to-end without hardware.
            FrameType::HwInfo => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                let body = b"model=CFI-1215A\n\
                             serial=TEST-SERIAL-123\n\
                             has_wlan_bt=1\n\
                             has_optical_out=0\n\
                             hw_model=CFI-1215A\n\
                             hw_machine=amd64\n\
                             os=FreeBSD 11.0\n\
                             ncpu=8\n\
                             physmem=13958643712\n";
                send_frame(&mut stream, FrameType::HwInfoAck, body);
            }
            FrameType::HwTemps => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                let body = b"cpu_temp=65\n\
                             soc_temp=72\n\
                             cpu_freq_mhz=3500\n\
                             soc_clock_mhz=0\n\
                             soc_power_mw=85000\n";
                send_frame(&mut stream, FrameType::HwTempsAck, body);
            }
            FrameType::HwPower => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                let body = b"operating_time_sec=7230\n\
                             operating_time_hours=2\n\
                             operating_time_minutes=0\n\
                             boot_count=0\n\
                             power_consumption_mw=0\n";
                send_frame(&mut stream, FrameType::HwPowerAck, body);
            }
            FrameType::AppLaunchBrowser => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                send_frame(&mut stream, FrameType::AppLaunchBrowserAck, b"");
            }
            FrameType::HwSetFanThreshold => {
                // Accept any body; real payload parses + clamps. Mock
                // just echoes success so integration tests can verify
                // the request-path plumbing without a real /dev/icc_fan.
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                send_frame(&mut stream, FrameType::HwSetFanThresholdAck, b"");
            }

            _ => {
                let mut discard = vec![0u8; hdr.body_len as usize];
                read_exact(&mut stream, &mut discard);
                send_error(&mut stream, "unsupported_frame");
            }
        }
    }
}

// ─── Minimal JSON helpers ─────────────────────────────────────────────────────
//
// Deliberately naive text-scan parsers — we don't want to pull serde_json
// into the mock since the mock is part of the test harness for the core
// that uses serde_json, and circularity in dep resolution has caused pain
// before. Good enough for the well-known manifest shape the transfer
// pipeline emits.
//
// Both helpers tolerate optional whitespace after the `:` so a future
// change from `serde_json::to_vec` (compact) to `to_string_pretty`
// doesn't silently stop matching fields.

fn extract_json_str(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let after_key = json.find(&needle)? + needle.len();
    // Skip any whitespace between `:` and the value's opening quote.
    let tail = &json.as_bytes()[after_key..];
    let ws_len = tail
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .count();
    let val_start = after_key + ws_len;
    // Require the value to be a quoted string; anything else (number,
    // bool, null) means the field isn't a string and we give up.
    if !json[val_start..].starts_with('"') {
        return None;
    }
    let val_start = val_start + 1;
    let end = json[val_start..].find('"')? + val_start;
    Some(json[val_start..end].to_string())
}

fn extract_json_u64(json: &str, field: &str) -> Option<u64> {
    let needle = format!("\"{field}\":");
    let start = json.find(&needle)? + needle.len();
    // Skip leading whitespace (in case the producer uses pretty-printed
    // JSON). `split(non-digit)` without this would yield the empty
    // string as the first element and parse() would fail silently,
    // giving back 0 / None for any field with a space after `:`.
    let trimmed = json[start..].trim_start_matches([' ', '\t', '\n', '\r']);
    trimmed
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|s| !s.is_empty())?
        .parse()
        .ok()
}
