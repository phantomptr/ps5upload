//! Process-manager RPC wrappers — list / kill.
//!
//! Thin Tauri command layer over `ps5upload_core::process_mgr`. The
//! renderer's Processes screen calls `process_list_get` on a ~2s poll and
//! `process_kill_pid` per row. `addr` is the management-port address
//! ("ip:9114"). Confirmation for killing a "system"-classified process
//! lives in the renderer; this side just relays.

use ps5upload_core::process_mgr::{process_kill, process_list};
use serde_json::Value as JsonValue;

/// Enumerate running processes (detailed: pid/name/comm/title_id/app_id/
/// memory/threads/kind + a `truncated` flag). Read-only.
#[tauri::command]
pub async fn process_list_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || process_list(&addr))
        .await
        .map_err(|e| format!("process_list task: {e}"))?
        .map(|r| serde_json::to_value(r).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("process_list: {e}"))
}

/// SIGKILL a process by pid. The payload guards self/kernel/init; the
/// renderer confirms before killing a "system" process.
#[tauri::command]
pub async fn process_kill_pid(addr: String, pid: i32) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || process_kill(&addr, pid))
        .await
        .map_err(|e| format!("process_kill task: {e}"))?
        .map(|ack| serde_json::to_value(ack).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("process_kill: {e}"))
}
