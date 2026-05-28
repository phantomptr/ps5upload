//! Tauri commands exposed to the renderer via `invoke('name', args)`.
//!
//! Every command is an async function with a stable signature: JSON-in /
//! JSON-out. Most are thin proxies to the ps5upload-engine HTTP API so
//! the UI can use a single `invoke()` call instead of juggling its own
//! fetch client with connection retry, auth headers, etc.
//!
//! Config/profile/queue/history persistence will also live here in a
//! later commit; the first pass (`config_load`, `config_save`) covers
//! enough for App.tsx to boot without Electron.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::engine;

// Generic JSON object passthrough — lets commands return heterogeneous
// responses from the engine without dragging every schema into Rust.
type JsonValue = serde_json::Value;

/// Shared HTTP client with explicit timeouts. Without these, a wedged
/// engine sidecar would leave every UI panel's invoke promise hanging
/// forever — visible to users as Library/Volumes/Stats panels stuck in
/// the loading state until app restart.
///
/// `connect_timeout` is short because the engine is local (loopback);
/// 2s is generous for "is the local sidecar up." `timeout` (60s) covers
/// the slowest expected payload-side operation for the "fast" trio of
/// endpoints (status, list-dir, volumes, etc.).
///
/// Long-running destructive endpoints (fs_delete, fs_copy, fs_move) go
/// through `http_client_long` instead — see its rationale below.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build engine HTTP client")
    })
}

/// Long-deadline HTTP client for the destructive-trio endpoints
/// (fs_delete, fs_copy, fs_move). The engine's handler for those holds
/// the HTTP request open for the entire payload-side run with a 1-hour
/// deadline of its own — deleting tens of thousands of files on PS5
/// UFS (e.g. a 46k-file game folder) routinely takes many minutes,
/// well past the 60 s ceiling on `http_client`. Pre-v2.18.4 those
/// requests timed out client-side with a misleading "engine request
/// failed: error sending request" message while the engine continued
/// running and the operation actually succeeded.
///
/// Match the engine's own deadline so a real wedge still surfaces as
/// a timeout (eventually) rather than hanging forever. The
/// connect_timeout stays short — if the local sidecar isn't there,
/// fail fast regardless of which endpoint family.
fn http_client_long() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(60 * 60))
            .build()
            .expect("failed to build long-deadline engine HTTP client")
    })
}

async fn get_json(url: &str) -> Result<JsonValue, String> {
    let resp = http_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("engine request failed: {e}"))?;
    let status = resp.status();
    // Read the body first so error responses can include the engine's
    // own diagnostic (e.g., "payload rejected FS_LIST_VOLUMES: ...").
    // Before this, a 502 from the engine collapsed to the useless
    // "engine returned HTTP 502 Bad Gateway" message in the UI.
    let body = resp
        .text()
        .await
        .map_err(|e| format!("engine response body read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("engine HTTP {status}: {body}"));
    }
    serde_json::from_str::<JsonValue>(&body)
        .map_err(|e| format!("engine returned invalid JSON: {e}"))
}

async fn post_json(url: &str, body: &JsonValue) -> Result<JsonValue, String> {
    post_json_with_client(http_client(), url, body).await
}

/// `post_json` variant that uses the long-deadline client. Use for the
/// destructive trio (fs_delete, fs_copy, fs_move) whose payload-side
/// runs routinely exceed `http_client`'s 60 s ceiling on large trees.
async fn post_json_long(url: &str, body: &JsonValue) -> Result<JsonValue, String> {
    post_json_with_client(http_client_long(), url, body).await
}

async fn post_json_with_client(
    client: &reqwest::Client,
    url: &str,
    body: &JsonValue,
) -> Result<JsonValue, String> {
    let resp = client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("engine request failed: {e}"))?;
    let status = resp.status();
    // Read the body first so error responses can include the engine's
    // own diagnostic, and so a 4xx/5xx with an empty or non-JSON body
    // doesn't collapse into "engine returned invalid JSON" — that
    // message hides the real HTTP status the user needs to debug. Same
    // pattern as get_json above.
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("engine response body read failed: {e}"))?;
    if !status.is_success() {
        // Try to extract the engine's `{"error":"..."}` field for a
        // cleaner message; fall back to the raw body text if it's not
        // JSON-shaped.
        let detail = serde_json::from_str::<JsonValue>(&body_text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| {
                if body_text.is_empty() {
                    "(empty body)".to_string()
                } else {
                    body_text.clone()
                }
            });
        return Err(format!("engine HTTP {status}: {detail}"));
    }
    serde_json::from_str::<JsonValue>(&body_text)
        .map_err(|e| format!("engine returned invalid JSON: {e}"))
}

#[tauri::command]
pub async fn ps5_volumes(addr: Option<String>) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = match addr {
        Some(a) => format!("{base}/api/ps5/volumes?addr={}", urlencoding(&a)),
        None => format!("{base}/api/ps5/volumes"),
    };
    get_json(&url).await
}

#[tauri::command]
pub async fn ps5_list_dir(
    path: String,
    addr: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let mut url = format!("{base}/api/ps5/list-dir?path={}", urlencoding(&path));
    if let Some(a) = addr {
        url.push_str(&format!("&addr={}", urlencoding(&a)));
    }
    if let Some(o) = offset {
        url.push_str(&format!("&offset={o}"));
    }
    if let Some(l) = limit {
        url.push_str(&format!("&limit={l}"));
    }
    get_json(&url).await
}

// ── Transfer jobs ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TransferFileReq {
    pub src: String,
    pub dest: String,
    pub addr: Option<String>,
    pub tx_id: Option<String>,
}

#[tauri::command]
pub async fn transfer_file(req: TransferFileReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/transfer/file");
    let body = serde_json::json!({
        "src": req.src,
        "dest": req.dest,
        "addr": req.addr,
        "tx_id": req.tx_id,
    });
    post_json(&url, &body).await
}

#[derive(Debug, Deserialize)]
pub struct TransferDirReq {
    pub src_dir: String,
    pub dest_root: String,
    pub addr: Option<String>,
    pub tx_id: Option<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Outbound bandwidth cap in MB/s. None or 0 means uncapped.
    #[serde(default)]
    pub bandwidth_cap_mbps: Option<f64>,
}

#[tauri::command]
pub async fn transfer_dir(req: TransferDirReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/transfer/dir");
    let body = serde_json::json!({
        "src_dir": req.src_dir,
        "dest_root": req.dest_root,
        "addr": req.addr,
        "tx_id": req.tx_id,
        "excludes": req.excludes,
        "bandwidth_cap_mbps": req.bandwidth_cap_mbps,
    });
    post_json(&url, &body).await
}

#[derive(Debug, Deserialize)]
pub struct TransferZipReq {
    pub zip_path: String,
    pub dest_root: String,
    pub addr: Option<String>,
    pub tx_id: Option<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub bandwidth_cap_mbps: Option<f64>,
}

/// Upload a `.zip`'s contents, decompressing on the host so files land
/// already extracted on the PS5. Proxies the engine's `/api/transfer/zip`.
///
/// Uses `post_json_long` rather than `post_json` because the engine
/// handler runs the central-directory plan synchronously **before**
/// returning a job_id — and zip-plan time scales with entry count and
/// disk-seek latency. A 70 GB game dump with 80–100k files on a slow
/// USB HDD has been observed to take >60s in plan, blowing the short
/// client timeout with the same "engine request failed: error sending
/// request" symptom that hit the destructive trio pre-2.18.4 (see
/// `http_client_long`'s rationale). Once plan completes the job is
/// asynchronous, so we only need the long deadline to cover the
/// front-loaded planning phase.
#[tauri::command]
pub async fn transfer_zip(req: TransferZipReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/transfer/zip");
    let body = serde_json::json!({
        "zip_path": req.zip_path,
        "dest_root": req.dest_root,
        "addr": req.addr,
        "tx_id": req.tx_id,
        "excludes": req.excludes,
        "bandwidth_cap_mbps": req.bandwidth_cap_mbps,
    });
    post_json_long(&url, &body).await
}

#[derive(Debug, Deserialize)]
pub struct ZipInspectReq {
    pub zip_path: String,
}

/// Preview a `.zip` (file count, compressed vs uncompressed size, embedded
/// game metadata) without extracting it. Proxies `/api/zip/inspect`.
///
/// Uses `post_json_long`: inspect reads the EOCD + central directory
/// (potentially tens of MB for a dump with 100k+ entries) from
/// user-supplied storage that may be a cold-cache external HDD. The
/// rare worst case is bounded by central-dir size, not by uncompressed
/// game size — so a 1h ceiling is over-generous but matches the
/// destructive-trio pattern (see `http_client_long`). The streaming
/// variant `zip_inspect_stream` supersedes this for the UI path with a
/// watchdog + progress events; this one stays for tests and any caller
/// that doesn't want to plumb a Channel through.
#[tauri::command]
pub async fn zip_inspect(req: ZipInspectReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/zip/inspect");
    let body = serde_json::json!({ "zip_path": req.zip_path });
    post_json_long(&url, &body).await
}

/// One progress tick from the engine's inspect-stream worker.
/// Mirrors the `data:` payload of the `progress` SSE event so the
/// renderer can show "Scanning archive… N entries" while the
/// central-directory walk is in flight.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ZipInspectProgress {
    pub entries_seen: u64,
}

/// Streaming variant of `zip_inspect`: subscribes to the engine's
/// `/api/zip/inspect/stream` SSE endpoint, forwards `progress` events
/// through a Tauri channel for live UI feedback, and returns the final
/// inspect result.
///
/// Watchdog: if no SSE chunk (including the engine's 1 s heartbeat)
/// arrives for `INSPECT_IDLE_TIMEOUT_SECS`, the call fails with
/// "engine stopped responding". That's the dead-man-switch the user
/// asked for — instead of a fixed wall-clock deadline, the success
/// criterion is *continuous forward signal*. A genuinely wedged engine
/// (no heartbeat, no progress) fails fast; a healthy engine that's
/// legitimately taking minutes to walk a 200 k-entry CD over a slow
/// network mount stays alive as long as it keeps signalling.
///
/// The renderer creates the channel as `new Channel<ZipInspectProgress>(cb)`
/// and passes it as `onProgress`; this Rust handler receives a typed
/// `tauri::ipc::Channel` and `send()`s each progress event back to JS.
#[tauri::command]
pub async fn zip_inspect_stream(
    req: ZipInspectReq,
    on_progress: tauri::ipc::Channel<ZipInspectProgress>,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/zip/inspect/stream");
    let body = serde_json::json!({ "zip_path": req.zip_path });
    post_sse_inspect_with_watchdog(&url, &body, on_progress).await
}

/// How long to wait for ANY SSE chunk (event or heartbeat) before
/// declaring the engine wedged. The engine's `KeepAlive::interval(1s)`
/// makes 30 s = 30× the expected heartbeat cadence, which leaves room
/// for a momentarily-blocked tokio runtime without triggering a false
/// "stopped responding" while still cutting off a real wedge promptly.
const INSPECT_IDLE_TIMEOUT_SECS: u64 = 30;

/// Drive the engine's `/api/zip/inspect/stream` SSE response with a
/// per-chunk idle watchdog.
///
/// `reqwest`'s `bytes_stream()` is byte-level, not event-level, so we
/// run a small SSE state machine here: accumulate bytes into a buffer,
/// look for the spec-defined `\n\n` event-boundary, split, parse the
/// `event:`/`data:` fields, and dispatch. Comments (`:` lines, which
/// is what our heartbeat is) reset the watchdog without invoking any
/// callback — that's exactly the "engine still alive" signal we need.
async fn post_sse_inspect_with_watchdog(
    url: &str,
    body: &serde_json::Value,
    on_progress: tauri::ipc::Channel<ZipInspectProgress>,
) -> Result<JsonValue, String> {
    use futures_util::StreamExt;

    let resp = http_client_long()
        .post(url)
        .header("accept", "text/event-stream")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("engine request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        // The streaming endpoint sends errors as a final `event: error`
        // line with HTTP 200, but a non-200 here means the engine
        // rejected the request shape outright (or the local sidecar is
        // down). Surface the body so the user sees the engine's own
        // message instead of an opaque status code.
        let body_text = resp
            .text()
            .await
            .map_err(|e| format!("engine response body read failed: {e}"))?;
        return Err(format!("engine HTTP {status}: {body_text}"));
    }

    let mut stream = resp.bytes_stream();
    let idle = Duration::from_secs(INSPECT_IDLE_TIMEOUT_SECS);
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        let chunk = match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => return Err(format!("engine stream error: {e}")),
            Ok(None) => {
                return Err(
                    "engine closed the inspect stream before sending a done/error event"
                        .to_string(),
                );
            }
            Err(_) => {
                return Err(format!(
                    "engine stopped responding (no SSE event for {INSPECT_IDLE_TIMEOUT_SECS}s)"
                ));
            }
        };
        buf.extend_from_slice(&chunk);

        // SSE event boundary is a blank line, i.e. two consecutive
        // newlines. Some servers (and our axum one) only emit `\n\n`,
        // but we accept `\r\n\r\n` too for safety against any future
        // proxy that rewrites line endings.
        while let Some(end) = find_event_boundary(&buf) {
            let event_bytes = buf.drain(..end.end).collect::<Vec<u8>>();
            // Strip the boundary itself before parsing.
            let block_bytes = &event_bytes[..end.start];
            let block = std::str::from_utf8(block_bytes)
                .map_err(|e| format!("engine sent non-UTF-8 SSE block: {e}"))?;
            if let Some(parsed) = parse_sse_block(block) {
                match parsed.event.as_deref() {
                    Some("progress") => {
                        if let Ok(p) = serde_json::from_str::<ZipInspectProgress>(&parsed.data) {
                            // Channel send is fire-and-forget: if the
                            // renderer is gone, we still let the inspect
                            // complete so the caller's awaited future
                            // resolves rather than dangling.
                            let _ = on_progress.send(p);
                        }
                    }
                    Some("done") => {
                        return serde_json::from_str::<JsonValue>(&parsed.data).map_err(|e| {
                            format!(
                                "engine sent invalid `done` JSON: {e} — body={}",
                                parsed.data
                            )
                        });
                    }
                    Some("error") => {
                        // Pull the {"error": "..."} envelope; fall back
                        // to the raw data if it doesn't parse.
                        let msg = serde_json::from_str::<JsonValue>(&parsed.data)
                            .ok()
                            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                            .unwrap_or_else(|| parsed.data.clone());
                        return Err(format!("engine reported: {msg}"));
                    }
                    _ => {
                        // Unknown event type — ignore but keep the
                        // watchdog reset (which already happened via
                        // chunk receipt).
                    }
                }
            }
        }
    }
}

/// Where in `buf` the next event-boundary lives, and where the boundary
/// itself ends. `start` excludes the boundary so the caller can slice
/// the block; `end` is past-the-end of the boundary so the caller can
/// drain.
struct EventBoundary {
    start: usize,
    end: usize,
}

fn find_event_boundary(buf: &[u8]) -> Option<EventBoundary> {
    if let Some(i) = memchr_pair(buf, b"\n\n") {
        return Some(EventBoundary {
            start: i,
            end: i + 2,
        });
    }
    if let Some(i) = memchr_pair(buf, b"\r\n\r\n") {
        return Some(EventBoundary {
            start: i,
            end: i + 4,
        });
    }
    None
}

fn memchr_pair(buf: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || buf.len() < needle.len() {
        return None;
    }
    buf.windows(needle.len()).position(|w| w == needle)
}

struct ParsedSseEvent {
    event: Option<String>,
    data: String,
}

/// Parse one SSE event block (everything between two blank lines).
/// We only need `event:` and `data:` for the inspect protocol; `id:`
/// and `retry:` aren't used. `data:` lines are joined with `\n` per
/// spec, in case the engine ever splits a long JSON payload.
fn parse_sse_block(block: &str) -> Option<ParsedSseEvent> {
    let mut event_name: Option<String> = None;
    let mut data_parts: Vec<String> = Vec::new();
    for line in block.split('\n') {
        // Tolerate CR left over by \r\n line endings.
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim_start_matches(' ').to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_parts.push(value.trim_start_matches(' ').to_string());
        }
    }
    if event_name.is_none() && data_parts.is_empty() {
        return None;
    }
    Some(ParsedSseEvent {
        event: event_name,
        data: data_parts.join("\n"),
    })
}

/// PS5 → host download. The engine walks the remote tree (or single
/// file) and pulls bytes via FS_READ on the management port. Response
/// is the standard `{ job_id }` shape — poll job_status to see
/// progress + errors. `kind` is the caller's known classification
/// ("file" | "folder") — saves a round-trip vs having the engine stat.
#[derive(Debug, Deserialize)]
pub struct TransferDownloadReq {
    pub src_path: String,
    pub dest_dir: String,
    pub addr: Option<String>,
    pub kind: String,
}

#[tauri::command]
pub async fn transfer_download(req: TransferDownloadReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/transfer/download");
    let body = serde_json::json!({
        "src_path": req.src_path,
        "dest_dir": req.dest_dir,
        "addr": req.addr,
        "kind": req.kind,
    });
    post_json(&url, &body).await
}

/// Resume-friendly folder upload: the engine reconciles local source
/// against PS5 destination first and only sends the delta. `mode` is
/// `"fast"` (size-only, default) or `"safe"` (size + BLAKE3). Response
/// is `{ job_id }` just like the other transfer handlers — poll via
/// `job_status` to see progress on the delta.
#[derive(Debug, Deserialize)]
pub struct TransferDirReconcileReq {
    pub src_dir: String,
    pub dest_root: String,
    pub addr: Option<String>,
    pub tx_id: Option<String>,
    pub mode: Option<String>, // "fast" | "safe"
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Outbound bandwidth cap in MB/s. None or 0 means uncapped.
    #[serde(default)]
    pub bandwidth_cap_mbps: Option<f64>,
}

// ── Destructive FS ops ──────────────────────────────────────────────────────
//
// Thin wrappers over the engine's /api/ps5/fs/* endpoints. Each one
// takes the JSON request body as-is and proxies to the engine, which
// does the actual payload round-trip on the PS5 mgmt port.

#[derive(Debug, Deserialize)]
pub struct FsPathReq {
    pub addr: Option<String>,
    pub path: String,
    /// Optional unique 64-bit id the client generates so it can poll
    /// progress / cancel the in-flight delete. Forwarded to the engine
    /// which forwards to the payload as the FS_DELETE frame's
    /// trace_id. Only used by `ps5_fs_delete`; other handlers that
    /// share this struct (e.g. `ps5_fs_mkdir`) ignore it.
    #[serde(default)]
    pub op_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FsMoveReq {
    pub addr: Option<String>,
    pub from: String,
    pub to: String,
    /// Optional unique 64-bit id the client generates so it can poll
    /// progress / cancel the in-flight copy. Forwarded to the engine
    /// which forwards to the payload as the FS_COPY frame's trace_id.
    /// Omit (or 0) for ops where progress/cancel isn't needed.
    #[serde(default)]
    pub op_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FsOpRefReq {
    pub addr: Option<String>,
    pub op_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct FsChmodReq {
    pub addr: Option<String>,
    pub path: String,
    pub mode: String,
    pub recursive: Option<bool>,
}

#[tauri::command]
pub async fn ps5_fs_delete(req: FsPathReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/delete");
    post_json_long(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "path": req.path,
            "op_id": req.op_id.unwrap_or(0),
        }),
    )
    .await
}

#[tauri::command]
pub async fn ps5_fs_move(req: FsMoveReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/move");
    post_json_long(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "from": req.from,
            "to": req.to,
            "op_id": req.op_id.unwrap_or(0),
        }),
    )
    .await
}

#[tauri::command]
pub async fn ps5_fs_copy(req: FsMoveReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/copy");
    post_json_long(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "from": req.from,
            "to": req.to,
            "op_id": req.op_id.unwrap_or(0),
        }),
    )
    .await
}

/// Snapshot the in-flight FS op identified by `op_id`. Returns the
/// payload's bytes_copied / total_bytes / cancel_requested so the
/// client can drive a per-byte progress + speed indicator while the
/// fs/copy call is still blocked. 404 from the engine surfaces as
/// an error string here so callers can stop polling.
#[tauri::command]
pub async fn ps5_fs_op_status(req: FsOpRefReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let mut url = format!("{base}/api/ps5/fs/op-status?op_id={}", req.op_id);
    if let Some(a) = req.addr {
        url.push_str(&format!("&addr={}", urlencoding(&a)));
    }
    get_json(&url).await
}

/// Ask the payload to cancel the in-flight FS op identified by
/// `op_id`. Returns `{ cancelled: bool }` — `false` means the op
/// already finished (or was never running), which is fine from the
/// client's perspective: the goal of "stop that copy" is met either
/// way.
#[tauri::command]
pub async fn ps5_fs_op_cancel(req: FsOpRefReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/op-cancel");
    post_json(
        &url,
        &serde_json::json!({ "addr": req.addr, "op_id": req.op_id }),
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct FsMountReq {
    pub addr: Option<String>,
    pub image_path: String,
    /// Optional leaf name under `/mnt/ps5upload/`. Mutually exclusive
    /// with `mount_point` — if both are provided, the engine prefers
    /// `mount_point` (full path wins over leaf). Kept for backward
    /// compatibility with 2.2.24 and earlier callers.
    #[serde(default)]
    pub mount_name: Option<String>,
    /// Optional full mount path. New in 2.2.25. When provided, the
    /// payload mounts at this exact path instead of the legacy
    /// `/mnt/ps5upload/<derived-name>/` location. Path must be under
    /// a writable root the payload's `is_path_allowed` accepts
    /// (`/data`, `/mnt/ext*`, `/mnt/usb*`, `/mnt/ps5upload/*`).
    #[serde(default)]
    pub mount_point: Option<String>,
    /// Mount the image read-only. New in 2.2.26. Default false (RW).
    /// When true, payload selects the RO LVD attach flag and the RO
    /// nmount third-arg flag (UFS magic 0x10000001 / MNT_RDONLY for
    /// exfatfs and pfs).
    #[serde(default)]
    pub read_only: Option<bool>,
}

#[tauri::command]
pub async fn ps5_fs_mount(req: FsMountReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/mount");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "image_path": req.image_path,
            "mount_name": req.mount_name,
            "mount_point": req.mount_point,
            "read_only": req.read_only,
        }),
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct FsUnmountReq {
    pub addr: Option<String>,
    pub mount_point: String,
}

/// Launch a registered title (re-exposed in 2.2.26). Payload-side
/// `launch_title` runs the triple-strategy chain
/// (`sceLncUtilLaunchApp` zeroed-param → NULL-param →
/// `sceSystemServiceLaunchApp`); errors here surface the composite
/// reason from that chain so the user can tell whether it was a
/// title-not-found or a kernel-side wedge.
#[derive(serde::Deserialize)]
pub struct AppLaunchReq {
    pub addr: Option<String>,
    pub title_id: String,
}

#[tauri::command]
pub async fn ps5_app_launch(req: AppLaunchReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/app/launch");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "title_id": req.title_id,
        }),
    )
    .await
}

/// Stage + register a game folder so it appears in the PS5 XMB
/// (re-exposed in 2.2.26). `src_path` is the directory that contains
/// `sce_sys/param.json` or `param.sfo` — works for direct folders
/// AND content under a mounted `/mnt/ps5upload/<name>/`. The payload
/// reports `{title_id, title_name, used_nullfs}` on the way back.
#[derive(serde::Deserialize)]
pub struct AppRegisterReq {
    pub addr: Option<String>,
    pub src_path: String,
    /// 2.2.26 opt-in DRM-type patcher.
    pub patch_drm_type: Option<bool>,
}

#[tauri::command]
pub async fn ps5_app_register(req: AppRegisterReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/app/register");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "src_path": req.src_path,
            "patch_drm_type": req.patch_drm_type,
        }),
    )
    .await
}

/// Reverse of `app_register`. Succeeds when the nullfs at
/// `/system_ex/app/<title_id>` is fully torn down, even if the Sony
/// AppUninstall API isn't available on the firmware.
#[derive(serde::Deserialize)]
pub struct AppUnregisterReq {
    pub addr: Option<String>,
    pub title_id: String,
}

#[tauri::command]
pub async fn ps5_app_unregister(req: AppUnregisterReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/app/unregister");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "title_id": req.title_id,
        }),
    )
    .await
}

#[tauri::command]
pub async fn ps5_fs_unmount(req: FsUnmountReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/unmount");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "mount_point": req.mount_point,
        }),
    )
    .await
}

#[tauri::command]
pub async fn ps5_fs_chmod(req: FsChmodReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/chmod");
    post_json(
        &url,
        &serde_json::json!({
            "addr": req.addr,
            "path": req.path,
            "mode": req.mode,
            "recursive": req.recursive.unwrap_or(false),
        }),
    )
    .await
}

#[tauri::command]
pub async fn ps5_fs_mkdir(req: FsPathReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/fs/mkdir");
    post_json(
        &url,
        &serde_json::json!({ "addr": req.addr, "path": req.path }),
    )
    .await
}

/* Hardware monitoring ---- */
fn addr_url(path: &str, addr: Option<&str>) -> String {
    let base = engine::url();
    match addr {
        Some(a) if !a.is_empty() => {
            format!("{base}{path}?addr={}", urlencoding(a))
        }
        _ => format!("{base}{path}"),
    }
}

#[tauri::command]
pub async fn ps5_hw_info(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/hw/info", addr.as_deref())).await
}

#[tauri::command]
pub async fn ps5_hw_temps(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/hw/temps", addr.as_deref())).await
}

#[tauri::command]
pub async fn ps5_hw_power(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/hw/power", addr.as_deref())).await
}

/// Recent PS5 kernel log (sysctl kern.msgbuf). Returned as
/// `{"text": "..."}` — UI renders verbatim in a scrollable monospace
/// area for diagnosing "why did the helper fail / what silently broke"
/// without making the user FTP/ssh into the console.
#[tauri::command]
pub async fn ps5_syslog_tail(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/syslog/tail", addr.as_deref())).await
}

/// Read the PS5's current system clock. Cheap; safe to call once on
/// the Hardware screen render and again right after a sync.
#[tauri::command]
pub async fn ps5_time_get(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/time/get", addr.as_deref())).await
}

/// Set the PS5's system clock to `target_unix_seconds` (UTC). The
/// payload bookends the set with a get-before + get-after so the
/// response can flag the "rc=0 but the clock didn't move" SDK-stub
/// no-op case (response field `stub_no_op: true`). Caller typically
/// passes `Math.floor(Date.now() / 1000)` to sync to PC time.
#[tauri::command]
pub async fn ps5_time_sync(
    addr: Option<String>,
    target_unix_seconds: i64,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/time/sync");
    post_json(
        &url,
        &serde_json::json!({
            "addr": addr,
            "target_unix_seconds": target_unix_seconds,
        }),
    )
    .await
}

/// Read all PS5 Date & Time state (timezone, DST, NTP flag,
/// date/time format, tzdata version, NTP-error counter, cached
/// NTP-tick) in one call. Returns the flat JSON the payload emits;
/// per-field availability flags let the UI grey out fields that
/// failed to read on this firmware. New in 2.10.0 — depends on the
/// novel DATE_* registry read path, see
/// reference_ps5_date_registry_keys.md for hardware status.
#[tauri::command]
pub async fn ps5_time_state_get(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/time/state/get", addr.as_deref())).await
}

/// Write a partial subset of PS5 Date & Time state. Pass any subset
/// of `tz_index`, `date_format`, `time_format`, `summer_policy`,
/// `set_auto` — None / omitted fields are NOT written. Response
/// surfaces per-field rc + err_code so the UI can show "set_auto
/// took, tz_index rejected" instead of one opaque ok/fail. Same
/// ucred-elevation envelope as ps5_time_sync.
#[tauri::command]
pub async fn ps5_time_state_set(
    addr: Option<String>,
    tz_index: Option<i32>,
    date_format: Option<i32>,
    time_format: Option<i32>,
    summer_policy: Option<i32>,
    set_auto: Option<i32>,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/time/state/set");
    // Build the request body with only present fields. serde's
    // skip_serializing_if + Option<T> on the engine side already
    // handles this, but we ALSO build the JSON conditionally so a
    // future engine-side change that flips defaults can't silently
    // start writing unintended fields. Belt-and-suspenders given
    // we're writing to a novel registry surface.
    let mut body = serde_json::Map::new();
    if let Some(a) = addr {
        body.insert("addr".into(), serde_json::Value::String(a));
    }
    if let Some(v) = tz_index {
        body.insert("tz_index".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = date_format {
        body.insert("date_format".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = time_format {
        body.insert("time_format".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = summer_policy {
        body.insert("summer_policy".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = set_auto {
        body.insert("set_auto".into(), serde_json::Value::Number(v.into()));
    }
    post_json(&url, &serde_json::Value::Object(body)).await
}

/// "Console Storage" aggregate matching what PS5 Settings shows
/// (added in 2.2.26). Returns total/free/used/reserved across
/// `/user effective + /system_data + /system_ex` plus per-partition
/// breakdown.
#[tauri::command]
pub async fn ps5_hw_storage(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/hw/storage", addr.as_deref())).await
}

/// Write the PS5 fan-turbo threshold. `threshold_c` is clamped on the
/// engine side (returns 400 BAD_REQUEST if out of the safe [45, 80]
/// range) before ever reaching the payload, so the UI gets a clear
/// error rather than a silent clamp.
#[tauri::command]
pub async fn ps5_hw_set_fan_threshold(
    addr: Option<String>,
    threshold_c: u8,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/hw/fan-threshold");
    post_json(
        &url,
        &serde_json::json!({ "addr": addr, "threshold_c": threshold_c }),
    )
    .await
}

/// ShadowMountPlus metadata self-healer control. `action` must be one
/// of `"start"`, `"run_now"`, `"set_poll"` — anything else is silently
/// a no-op inside the payload. `interval` is only consulted on
/// `set_poll`; clamped to [5, 600] by the payload. The UI calls this
/// the first time the user clicks "Enable" in the SMP Meta Heal panel
/// (action=start), then occasionally on slider changes (set_poll) or
/// manual triggers (run_now).
#[tauri::command]
pub async fn ps5_smp_meta_control(
    addr: Option<String>,
    action: String,
    interval: Option<i32>,
) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/ps5/smp-meta/control");
    let mut body = serde_json::Map::new();
    if let Some(a) = addr {
        body.insert("addr".into(), serde_json::Value::String(a));
    }
    body.insert("action".into(), serde_json::Value::String(action));
    if let Some(v) = interval {
        body.insert("interval".into(), serde_json::Value::Number(v.into()));
    }
    post_json(&url, &serde_json::Value::Object(body)).await
}

/// Snapshot of the SMP-meta worker's stats. Safe to call even before
/// the worker is started — returns `running:false` with zeroed
/// counters. The panel polls this on a slow tick (10–30 s) so the
/// user can see "N icons healed this sweep" land without manually
/// refreshing.
#[tauri::command]
pub async fn ps5_smp_meta_stats(addr: Option<String>) -> Result<JsonValue, String> {
    get_json(&addr_url("/api/ps5/smp-meta/stats", addr.as_deref())).await
}

#[tauri::command]
pub async fn transfer_dir_reconcile(req: TransferDirReconcileReq) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/transfer/dir-reconcile");
    let body = serde_json::json!({
        "src_dir": req.src_dir,
        "dest_root": req.dest_root,
        "addr": req.addr,
        "tx_id": req.tx_id,
        "mode": req.mode,
        "excludes": req.excludes,
        "bandwidth_cap_mbps": req.bandwidth_cap_mbps,
    });
    post_json(&url, &body).await
}

#[tauri::command]
pub async fn job_status(job_id: String) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/jobs/{job_id}");
    get_json(&url).await
}

/// Pull recent engine log lines from the sidecar's in-memory ring so the
/// renderer can mirror them into its Log tab. `since` is the highest
/// `seq` the caller has already seen; pass `0` on first call. Response
/// shape: `{ entries: [...], next_seq: N }`.
#[tauri::command]
pub async fn engine_logs_tail(since: u64) -> Result<JsonValue, String> {
    let base = engine::url();
    let url = format!("{base}/api/engine-logs?since={since}");
    get_json(&url).await
}

// ── PKG install ─────────────────────────────────────────────────────────────

/// Parse a single `.pkg` file's header. Returns metadata: content_id,
/// title (from PARAM.SFO), category, ICON0.PNG (base64), warnings.
/// Files with non-stock magic surface as `kind:"unknown"` with a warning
/// rather than a hard error so the user can still attempt install.
#[tauri::command]
pub async fn pkg_metadata(path: String) -> Result<JsonValue, String> {
    let url = format!("{}/api/pkg/parse", engine::url());
    post_json(&url, &serde_json::json!({ "path": path })).await
}

/// Same as `pkg_metadata` but auto-detects sibling split parts
/// (`<root>.0`, `<root>.1`, ...) in the same directory and returns
/// the assembled total size + per-part list.
#[tauri::command]
pub async fn pkg_metadata_split(path: String) -> Result<JsonValue, String> {
    let url = format!("{}/api/pkg/parse-split", engine::url());
    post_json(&url, &serde_json::json!({ "path": path })).await
}

/// Pre-flight folder diff: walks local + remote, returns the
/// "what would actually change" stats without uploading. UI uses
/// this to show "X new, Y replaced" before the user commits to a
/// multi-GB transfer.
#[tauri::command]
pub async fn transfer_dir_diff_preview(
    src_dir: String,
    dest_root: String,
    addr: String,
    excludes: Vec<String>,
) -> Result<JsonValue, String> {
    let url = format!("{}/api/transfer/dir-diff-preview", engine::url());
    post_json(
        &url,
        &serde_json::json!({
            "src_dir": src_dir,
            "dest_root": dest_root,
            "addr": addr,
            "excludes": excludes,
        }),
    )
    .await
}

/// Inspect a local UFS2 image file (`.ffpkg`, `.ufs`) without
/// uploading. Returns superblock info, root directory contents, and
/// PARAM.SFO metadata when sce_sys/param.sfo exists. Read-only —
/// safe to run on any file the user picks.
#[tauri::command]
pub async fn ffpkg_inspect(path: String) -> Result<JsonValue, String> {
    let url = format!("{}/api/ffpkg/inspect", engine::url());
    post_json(&url, &serde_json::json!({ "path": path })).await
}

/// Extract a file or subtree from a local `.ffpkg` to a local dir.
/// `inner_path` is slash-separated and refers to the path inside the
/// image (empty string = whole image root). Read-only on the image.
#[tauri::command]
pub async fn ffpkg_extract(
    ffpkg_path: String,
    inner_path: String,
    dest_dir: String,
) -> Result<JsonValue, String> {
    let url = format!("{}/api/ffpkg/extract", engine::url());
    post_json(
        &url,
        &serde_json::json!({
            "ffpkg_path": ffpkg_path,
            "inner_path": inner_path,
            "dest_dir": dest_dir,
        }),
    )
    .await
}

/// Kick off an install. Returns the session_id, the HTTP URL the PS5
/// will fetch from, and the BGFT task_id. Caller polls `pkg_install_status`
/// until phase=done|error.
#[tauri::command]
pub async fn pkg_install_start(
    ps5_addr: String,
    path: Option<String>,
    split_root: Option<String>,
    package_type_override: Option<String>,
    local_ps5_path: Option<String>,
) -> Result<JsonValue, String> {
    let url = format!("{}/api/pkg/install/start", engine::url());
    let body = serde_json::json!({
        "ps5_addr": ps5_addr,
        "path": path,
        "split_root": split_root,
        "package_type_override": package_type_override,
        "local_ps5_path": local_ps5_path,
    });
    post_json(&url, &body).await
}

/// Poll an in-flight install for status. Cheap; called every 1-2s.
#[tauri::command]
pub async fn pkg_install_status(session: String) -> Result<JsonValue, String> {
    let url = format!(
        "{}/api/pkg/install/status?session={}",
        engine::url(),
        urlencoding(&session)
    );
    get_json(&url).await
}

/// Cancel an in-flight install. Stops the host-side HTTP listener
/// for this session; BGFT on the PS5 will surface a download error
/// in its notifications when it sees the stream drop.
#[tauri::command]
pub async fn pkg_install_cancel(session: String) -> Result<JsonValue, String> {
    let url = format!("{}/api/pkg/install/cancel", engine::url());
    post_json(&url, &serde_json::json!({ "session": session })).await
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Tiny URL-encode that handles the characters we feed into query strings
/// (slashes, colons, spaces). We don't take a dep on the `urlencoding`
/// crate — a 15-char allow-set is plenty.
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~".contains(c) {
                c.to_string()
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .as_bytes()
                    .iter()
                    .map(|b| format!("%{b:02X}"))
                    .collect::<String>()
            }
        })
        .collect()
}
