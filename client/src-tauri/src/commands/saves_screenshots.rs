//! Tauri command wrappers for save data + screenshot listing.

use ps5upload_core::saves::{list_saves, list_screenshots};
use serde_json::Value as JsonValue;

#[tauri::command]
pub async fn saves_list(addr: String, user_id: Option<i32>) -> Result<JsonValue, String> {
    let uid = user_id.unwrap_or(0);
    tokio::task::spawn_blocking(move || list_saves(&addr, uid))
        .await
        .map_err(|e| format!("saves task: {e}"))?
        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("saves: {e}"))
}

#[tauri::command]
pub async fn screenshots_list(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || list_screenshots(&addr))
        .await
        .map_err(|e| format!("screenshots task: {e}"))?
        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("screenshots: {e}"))
}
