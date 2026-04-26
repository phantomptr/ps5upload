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

use serde::Deserialize;

use crate::engine;

// Generic JSON object passthrough — lets commands return heterogeneous
// responses from the engine without dragging every schema into Rust.
type JsonValue = serde_json::Value;

async fn get_json(url: &str) -> Result<JsonValue, String> {
    let client = reqwest::Client::new();
    let resp = client
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
    let client = reqwest::Client::new();
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
    });
    post_json(&url, &body).await
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
    post_json(
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
    post_json(
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
    post_json(
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
    #[serde(default)]
    pub mount_name: Option<String>,
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
        }),
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct FsUnmountReq {
    pub addr: Option<String>,
    pub mount_point: String,
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
