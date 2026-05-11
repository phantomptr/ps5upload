//! Tauri command wrappers for the payload-side filesystem index.

use ps5upload_core::search_index::{
    index_cancel, index_start, index_status, search_index, SearchQuery,
};
use serde_json::Value as JsonValue;

#[tauri::command]
pub async fn fs_index_start(addr: String, roots: Option<Vec<String>>) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || {
        let r: Vec<&str> = roots
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        index_start(&addr, &r)
    })
    .await
    .map_err(|e| format!("index_start task: {e}"))?
    .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
    .map_err(|e| format!("index_start: {e}"))
}

#[tauri::command]
pub async fn fs_index_status(addr: String) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || index_status(&addr))
        .await
        .map_err(|e| format!("index_status task: {e}"))?
        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("index_status: {e}"))
}

#[tauri::command]
pub async fn fs_search_index(
    addr: String,
    query: String,
    size_min: Option<u64>,
    size_max: Option<u64>,
    limit: Option<u32>,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || {
        let q = SearchQuery {
            query,
            size_min: size_min.unwrap_or(0),
            size_max: size_max.unwrap_or(0),
            limit: limit.unwrap_or(200),
        };
        search_index(&addr, &q)
    })
    .await
    .map_err(|e| format!("search task: {e}"))?
    .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
    .map_err(|e| format!("search: {e}"))
}

#[tauri::command]
pub async fn fs_index_cancel(addr: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || index_cancel(&addr))
        .await
        .map_err(|e| format!("cancel task: {e}"))?
        .map_err(|e| format!("cancel: {e}"))
}
