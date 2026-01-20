use ps5upload_core::config::AppConfig;
use ps5upload_core::history::{
    clear_history_to, load_history_from, save_history_to, HistoryData, TransferRecord,
};
use ps5upload_core::profiles::{load_profiles_from, save_profiles_to, ProfilesData};
use ps5upload_core::protocol::{list_storage, StorageLocation};
use ps5upload_core::queue::{load_queue_from, save_queue_to, QueueData};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use tauri::AppHandle;

use crate::paths::resolve_paths;

const TRANSFER_PORT: u16 = 9113;

#[tauri::command]
pub fn app_version() -> String {
    include_str!("../../../VERSION").trim().to_string()
}

#[tauri::command]
pub fn config_load(app: AppHandle) -> AppConfig {
    let paths = resolve_paths(&app);
    AppConfig::load_from(&paths.config)
}

#[tauri::command]
pub fn config_save(app: AppHandle, config: AppConfig) -> Result<(), String> {
    let paths = resolve_paths(&app);
    config.save_to(&paths.config).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn storage_list(ip: String) -> Result<Vec<StorageLocation>, String> {
    tauri::async_runtime::block_on(async {
        list_storage(&ip, TRANSFER_PORT)
            .await
            .map_err(|err| err.to_string())
    })
}

#[tauri::command]
pub fn port_check(ip: String, port: u16) -> Result<bool, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    let addr: SocketAddr = format!("{}:{}", ip.trim(), port)
        .parse()
        .map_err(|_| "Invalid address".to_string())?;
    let timeout = Duration::from_secs(2);
    TcpStream::connect_timeout(&addr, timeout)
        .map(|_| true)
        .or_else(|err| {
            if err.kind() == std::io::ErrorKind::TimedOut
                || err.kind() == std::io::ErrorKind::ConnectionRefused
                || err.kind() == std::io::ErrorKind::ConnectionReset
            {
                Ok(false)
            } else {
                Err(err.to_string())
            }
        })
}

#[tauri::command]
pub fn profiles_load(app: AppHandle) -> ProfilesData {
    let paths = resolve_paths(&app);
    load_profiles_from(&paths.profiles, Some(&paths.profiles_json))
}

#[tauri::command]
pub fn profiles_save(app: AppHandle, data: ProfilesData) -> Result<(), String> {
    let paths = resolve_paths(&app);
    save_profiles_to(&data, &paths.profiles).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn queue_load(app: AppHandle) -> QueueData {
    let paths = resolve_paths(&app);
    load_queue_from(&paths.queue)
}

#[tauri::command]
pub fn queue_save(app: AppHandle, data: QueueData) -> Result<(), String> {
    let paths = resolve_paths(&app);
    save_queue_to(&data, &paths.queue).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn history_load(app: AppHandle) -> HistoryData {
    let paths = resolve_paths(&app);
    load_history_from(&paths.history)
}

#[tauri::command]
pub fn history_add(app: AppHandle, record: TransferRecord) -> Result<(), String> {
    let paths = resolve_paths(&app);
    let mut data = load_history_from(&paths.history);
    data.records.push(record);
    save_history_to(&data, &paths.history).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn history_clear(app: AppHandle) -> Result<(), String> {
    let paths = resolve_paths(&app);
    let mut data = load_history_from(&paths.history);
    clear_history_to(&mut data, &paths.history).map_err(|err| err.to_string())
}
