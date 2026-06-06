//! Tauri command wrappers for the diagnostics RPCs.

use ps5upload_core::diagnostics::{
    appdb_query, crc32_file, fs_write_bytes, klog_read, lwfs_mount, net_interfaces, net_speed_test,
    peripheral_control, pkg_direct_mount, proc_modules, shell_run, ufs_fsck, PeripheralAction,
};
use ps5upload_core::fs_ops::{fs_hash, fs_read_with_timeout};
use ps5upload_core::hw::proc_list;
use serde_json::Value as JsonValue;
use std::time::Duration;

#[tauri::command]
pub async fn klog_chunk(addr: String, max_bytes: Option<u32>) -> Result<String, String> {
    let cap = max_bytes.unwrap_or(16 * 1024);
    tokio::task::spawn_blocking(move || klog_read(&addr, cap))
        .await
        .map_err(|e| format!("klog task: {e}"))?
        .map_err(|e| format!("klog: {e}"))
}

#[tauri::command]
pub async fn net_interfaces_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || net_interfaces(&addr))
        .await
        .map_err(|e| format!("net task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("net: {e}"))
}

#[tauri::command]
pub async fn peripheral_eject(addr: String) -> Result<JsonValue, String> {
    invoke_periph(addr, PeripheralAction::EjectDisc, 0).await
}
#[tauri::command]
pub async fn peripheral_bd_off(addr: String) -> Result<JsonValue, String> {
    invoke_periph(addr, PeripheralAction::BdPowerOff, 0).await
}
#[tauri::command]
pub async fn peripheral_bd_on(addr: String) -> Result<JsonValue, String> {
    invoke_periph(addr, PeripheralAction::BdPowerOn, 0).await
}
#[tauri::command]
pub async fn peripheral_usb_off(addr: String, port: i32) -> Result<JsonValue, String> {
    invoke_periph(addr, PeripheralAction::UsbPortOff, port).await
}
#[tauri::command]
pub async fn peripheral_usb_on(addr: String, port: i32) -> Result<JsonValue, String> {
    invoke_periph(addr, PeripheralAction::UsbPortOn, port).await
}

async fn invoke_periph(
    addr: String,
    action: PeripheralAction,
    port: i32,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || peripheral_control(&addr, action, port))
        .await
        .map_err(|e| format!("periph task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("periph: {e}"))
}

/// Full running-process list (pid + name) for the bug-report PS5 snapshot.
/// Distinct from `proc_modules_get` (which lists a single process's loaded
/// modules). Read-only allproc walk; cheap.
#[tauri::command]
pub async fn proc_list_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || proc_list(&addr))
        .await
        .map_err(|e| format!("proc_list task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("proc_list: {e}"))
}

#[tauri::command]
pub async fn proc_modules_get(addr: String, pid: Option<i32>) -> Result<JsonValue, String> {
    let p = pid.unwrap_or(0);
    tokio::task::spawn_blocking(move || proc_modules(&addr, p))
        .await
        .map_err(|e| format!("modules task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("modules: {e}"))
}

#[tauri::command]
pub async fn shell_run_cmd(
    addr: String,
    cmd: String,
    session_id: Option<String>,
    cwd: Option<String>,
    timeout_secs: Option<u32>,
) -> Result<JsonValue, String> {
    let t = timeout_secs.unwrap_or(30);
    tokio::task::spawn_blocking(move || {
        shell_run(&addr, &cmd, session_id.as_deref(), cwd.as_deref(), t)
    })
    .await
    .map_err(|e| format!("shell task: {e}"))?
    .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
    .map_err(|e| format!("shell: {e}"))
}

#[tauri::command]
pub async fn crc32_file_get(addr: String, path: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || crc32_file(&addr, &path))
        .await
        .map_err(|e| format!("crc32 task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("crc32: {e}"))
}

#[tauri::command]
pub async fn appdb_query_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || appdb_query(&addr))
        .await
        .map_err(|e| format!("appdb task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("appdb: {e}"))
}

#[tauri::command]
pub async fn net_speed_test_run(
    addr: String,
    round_trips: Option<u32>,
) -> Result<JsonValue, String> {
    let n = round_trips.unwrap_or(32);
    tokio::task::spawn_blocking(move || net_speed_test(&addr, n))
        .await
        .map_err(|e| format!("speedtest task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("speedtest: {e}"))
}

#[tauri::command]
pub async fn pkg_direct_mount_run(
    addr: String,
    pkg_path: String,
    mount_point: Option<String>,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || pkg_direct_mount(&addr, &pkg_path, mount_point.as_deref()))
        .await
        .map_err(|e| format!("pkgmount task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("pkgmount: {e}"))
}

/// Read up to `max_bytes` of a PS5-side file as base64. The renderer
/// uses this to preview small text files inline (decoding to UTF-8)
/// or render small images via `data:image/...;base64,<…>` URLs.
/// Capped at 256 KB on the payload side; the cap here mirrors that.
/// Compute the BLAKE3 hash of a PS5-side file. Mirrors the FS_HASH
/// frame used internally by the reconcile path; surfaces it directly
/// for the verifier panel + any caller that wants crypto-strength
/// integrity (CRC32 catches casual corruption but not deliberate
/// tampering).
#[tauri::command]
pub async fn fs_blake3_hash(addr: String, path: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || fs_hash(&addr, &path))
        .await
        .map_err(|e| format!("hash task: {e}"))?
        .map(|h| {
            serde_json::json!({
                "path": h.path,
                "size": h.size,
                "hash": h.hash,
            })
        })
        .map_err(|e| format!("hash: {e}"))
}

#[tauri::command]
pub async fn fs_read_preview(
    addr: String,
    path: String,
    max_bytes: Option<u64>,
) -> Result<JsonValue, String> {
    let cap = max_bytes.unwrap_or(256 * 1024).min(256 * 1024);
    let bytes = tokio::task::spawn_blocking(move || {
        fs_read_with_timeout(&addr, &path, 0, cap, Some(Duration::from_secs(10)))
    })
    .await
    .map_err(|e| format!("fs_read task: {e}"))?
    .map_err(|e| format!("fs_read: {e}"))?;
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(serde_json::json!({
        "size": bytes.len(),
        "base64": b64,
    }))
}

#[tauri::command]
pub async fn ufs_fsck_run(
    addr: String,
    device: String,
    repair: Option<bool>,
) -> Result<JsonValue, String> {
    let r = repair.unwrap_or(false);
    tokio::task::spawn_blocking(move || ufs_fsck(&addr, &device, r))
        .await
        .map_err(|e| format!("fsck task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("fsck: {e}"))
}

/// Write a small file (≤256 KB) atomically. `bytes_b64` is the
/// already-base64-encoded payload — the renderer is in the best
/// position to base64-encode (Web APIs do this trivially) and it
/// avoids round-tripping raw binary through Tauri's IPC layer.
#[tauri::command]
pub async fn fs_write_bytes_run(
    addr: String,
    path: String,
    bytes_b64: String,
    create_only: Option<bool>,
) -> Result<JsonValue, String> {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(bytes_b64.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    let only = create_only.unwrap_or(false);
    tokio::task::spawn_blocking(move || fs_write_bytes(&addr, &path, &raw, only))
        .await
        .map_err(|e| format!("write task: {e}"))?
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("write: {e}"))
}

#[tauri::command]
pub async fn lwfs_mount_run(
    addr: String,
    patch_path: String,
    mount_point: Option<String>,
    title_id: Option<String>,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || {
        lwfs_mount(
            &addr,
            &patch_path,
            mount_point.as_deref(),
            title_id.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("lwfs task: {e}"))?
    .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
    .map_err(|e| format!("lwfs: {e}"))
}
