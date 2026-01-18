use ps5upload_core::config::AppConfig;
use ps5upload_core::protocol::{list_storage, StorageLocation};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const TRANSFER_PORT: u16 = 9113;

fn config_path(app: &AppHandle) -> PathBuf {
    if let Ok(path) = std::env::var("PS5UPLOAD_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    if cfg!(debug_assertions) {
        if let Ok(current) = std::env::current_dir() {
            let temp_dir = current.join("..").join("..").join("temp");
            if temp_dir.is_dir() {
                return temp_dir.join("ps5upload.ini");
            }
        }
    }

    if let Ok(dir) = app.path().app_config_dir() {
        return dir.join("ps5upload.ini");
    }

    PathBuf::from("ps5upload.ini")
}

#[tauri::command]
pub fn app_version() -> String {
    include_str!("../../../VERSION").trim().to_string()
}

#[tauri::command]
pub fn config_load(app: AppHandle) -> AppConfig {
    AppConfig::load_from(&config_path(&app))
}

#[tauri::command]
pub fn config_save(app: AppHandle, config: AppConfig) -> Result<(), String> {
    config
        .save_to(&config_path(&app))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn storage_list(ip: String) -> Result<Vec<StorageLocation>, String> {
    tauri::async_runtime::block_on(async {
        list_storage(&ip, TRANSFER_PORT)
            .await
            .map_err(|err| err.to_string())
    })
}
