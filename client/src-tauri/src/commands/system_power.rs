//! Power control RPC wrappers — reboot / shutdown / standby / tick.
//!
//! Thin Tauri command layer over `ps5upload_core::system_control`.
//! Per-action confirmation lives in the renderer; this side just
//! relays the action and translates the SystemControlAck into a
//! flat JSON the renderer can render directly.
//!
//! `addr` here is the management-port address ("ip:9114"). Renderer
//! constructs it via the existing `${host}:9114` pattern.

use ps5upload_core::system_control::{power_telemetry, system_control, PowerAction};
use ps5upload_core::users::user_list;
use serde_json::Value as JsonValue;

#[tauri::command]
pub async fn power_reboot(addr: String) -> Result<JsonValue, String> {
    invoke_action(addr, PowerAction::Reboot).await
}

#[tauri::command]
pub async fn power_shutdown(addr: String) -> Result<JsonValue, String> {
    invoke_action(addr, PowerAction::Shutdown).await
}

#[tauri::command]
pub async fn power_standby(addr: String) -> Result<JsonValue, String> {
    invoke_action(addr, PowerAction::Standby).await
}

#[tauri::command]
pub async fn power_tick(addr: String) -> Result<JsonValue, String> {
    invoke_action(addr, PowerAction::Tick).await
}

/// Fetch the PS5's lifetime ICC telemetry (operating seconds, boot
/// cycles, thermal alerts, power-up cause). Read-only.
#[tauri::command]
pub async fn power_telemetry_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || power_telemetry(&addr))
        .await
        .map_err(|e| format!("telemetry task: {e}"))?
        .map(|t| serde_json::to_value(t).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("telemetry: {e}"))
}

/// Enumerate user accounts on the connected PS5.
#[tauri::command]
pub async fn user_list_get(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || user_list(&addr))
        .await
        .map_err(|e| format!("user_list task: {e}"))?
        .map(|u| serde_json::to_value(u).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("user_list: {e}"))
}

async fn invoke_action(addr: String, action: PowerAction) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || system_control(&addr, action))
        .await
        .map_err(|e| format!("power task: {e}"))?
        .map(|ack| serde_json::to_value(ack).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("power: {e}"))
}
