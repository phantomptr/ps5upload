use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

use ps5upload_core::message::ReleaseInfo;
use ps5upload_core::update::{
    build_pending_update, current_asset_name, download_asset, extract_zip, fetch_latest_release,
    fetch_release_by_tag, spawn_update_helper,
};

use crate::state::AppState;

#[derive(Clone, Serialize)]
pub struct UpdateDownloadResult {
    pub path: String,
}

#[tauri::command]
pub async fn update_check(include_prerelease: bool) -> Result<ReleaseInfo, String> {
    fetch_latest_release(include_prerelease)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn update_check_tag(tag: String) -> Result<ReleaseInfo, String> {
    fetch_release_by_tag(&tag)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn update_download_asset(url: String, dest_path: String) -> Result<UpdateDownloadResult, String> {
    download_asset(&url, &dest_path)
        .await
        .map_err(|err| err.to_string())?;
    Ok(UpdateDownloadResult { path: dest_path })
}

#[tauri::command]
pub fn update_current_asset_name() -> Result<String, String> {
    current_asset_name()
}

#[tauri::command]
pub fn update_prepare_self(
    app_handle: AppHandle,
    state: State<AppState>,
    asset_url: String,
) -> Result<(), String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let tmp_root = std::env::temp_dir().join(format!("ps5upload_update_{}", stamp));
    std::fs::create_dir_all(&tmp_root).map_err(|err| err.to_string())?;
    let zip_path = tmp_root.join("update.zip");
    let extract_dir = tmp_root.join("extracted");
    let url = asset_url.clone();

    let pending_update = state.pending_update.clone();
    std::thread::spawn(move || {
        let result = tauri::async_runtime::block_on(async {
            download_asset(&url, &zip_path.display().to_string()).await?;
            extract_zip(&zip_path, &extract_dir)?;
            build_pending_update(&extract_dir)
        });

        match result {
            Ok(pending) => {
                if let Ok(mut guard) = pending_update.lock() {
                    *guard = Some(pending);
                }
                let _ = app_handle.emit("update_ready", ());
            }
            Err(err) => {
                let _ = app_handle.emit("update_error", err.to_string());
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub fn update_apply_self(state: State<AppState>) -> Result<(), String> {
    let pending = {
        let mut guard = state
            .pending_update
            .lock()
            .map_err(|_| "Update state locked".to_string())?;
        guard.take()
    };
    let Some(pending) = pending else {
        return Err("No pending update".to_string());
    };
    spawn_update_helper(&pending).map_err(|err| err.to_string())?;
    std::process::exit(0);
}
