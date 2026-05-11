//! Tauri command wrappers for app lifecycle + rich toast.

use ps5upload_core::app_lifecycle::{app_lifecycle, toast_send, AppAction, ToastRequest};
use serde_json::Value as JsonValue;

#[tauri::command]
pub async fn app_suspend(addr: String, app_id: u32) -> Result<JsonValue, String> {
    invoke_lifecycle(addr, AppAction::Suspend, app_id).await
}
#[tauri::command]
pub async fn app_resume(addr: String, app_id: u32) -> Result<JsonValue, String> {
    invoke_lifecycle(addr, AppAction::Resume, app_id).await
}
#[tauri::command]
pub async fn app_kill(addr: String, app_id: u32) -> Result<JsonValue, String> {
    invoke_lifecycle(addr, AppAction::Kill, app_id).await
}
#[tauri::command]
pub async fn app_list_running(addr: String) -> Result<JsonValue, String> {
    invoke_lifecycle(addr, AppAction::List, 0).await
}

async fn invoke_lifecycle(
    addr: String,
    action: AppAction,
    app_id: u32,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || app_lifecycle(&addr, action, app_id))
        .await
        .map_err(|e| format!("app task: {e}"))?
        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::json!({})))
        .map_err(|e| format!("app: {e}"))
}

#[tauri::command]
pub async fn toast_push(
    addr: String,
    title: String,
    subtitle: Option<String>,
    icon: Option<String>,
    action_url: Option<String>,
) -> Result<JsonValue, String> {
    tokio::task::spawn_blocking(move || {
        let req = ToastRequest {
            title,
            subtitle: subtitle.unwrap_or_default(),
            icon: icon.unwrap_or_default(),
            action_url: action_url.unwrap_or_default(),
        };
        toast_send(&addr, &req)
    })
    .await
    .map_err(|e| format!("toast task: {e}"))?
    .map(|a| serde_json::to_value(a).unwrap_or(serde_json::json!({})))
    .map_err(|e| format!("toast: {e}"))
}
