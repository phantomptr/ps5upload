//! Tauri command wrappers for save data + screenshot + video-clip listing.

use ps5upload_core::saves::{list_saves, list_screenshots, list_videos};
use serde_json::Value as JsonValue;

#[tauri::command]
pub async fn saves_list(addr: String, user_id: Option<i32>) -> Result<JsonValue, String> {
    let uid = user_id.unwrap_or(0);
    tokio::task::spawn_blocking(move || list_saves(&addr, uid))
        .await
        .map_err(|e| format!("saves task: {e}"))?
        .map_err(|e| format!("saves: {e}"))
        .and_then(super::reply_json)
}

#[tauri::command]
pub async fn screenshots_list(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || list_screenshots(&addr))
        .await
        .map_err(|e| format!("screenshots task: {e}"))?
        .map_err(|e| format!("screenshots: {e}"))
        .and_then(super::reply_json)
}

#[tauri::command]
pub async fn videos_list(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || list_videos(&addr))
        .await
        .map_err(|e| format!("videos task: {e}"))?
        .map_err(|e| format!("videos: {e}"))
        .and_then(super::reply_json)
}
