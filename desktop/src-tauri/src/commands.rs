use ps5upload_core::config::AppConfig;
use ps5upload_core::history::{
    clear_history_to, load_history_from, save_history_to, HistoryData, TransferRecord,
};
use ps5upload_core::profiles::{load_profiles_from, save_profiles_to, ProfilesData};
use ps5upload_core::protocol::{list_storage, StorageLocation};
use ps5upload_core::queue::{load_queue_from, save_queue_to, QueueData};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

use crate::paths::resolve_paths;
use crate::state::AppState;

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
pub fn config_update(app: AppHandle, state: State<AppState>, config: AppConfig) -> Result<(), String> {
    if let Ok(mut guard) = state.config_pending.lock() {
        *guard = Some(config);
    }
    if state
        .config_saver_active
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let next = {
                    let state = handle.state::<AppState>();
                    let mut guard = state.config_pending.lock().unwrap();
                    guard.take()
                };
                if let Some(next_config) = next {
                    let paths = resolve_paths(&handle);
                    let _ = next_config.save_to(&paths.config);
                    continue;
                }
                let state = handle.state::<AppState>();
                state.config_saver_active.store(false, Ordering::Relaxed);
                break;
            }
        });
    }
    Ok(())
}

#[tauri::command]
pub async fn storage_list(ip: String) -> Result<Vec<StorageLocation>, String> {
    list_storage(&ip, TRANSFER_PORT)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn port_check(ip: String, port: u16) -> Result<bool, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    let addr: SocketAddr = format!("{}:{}", ip.trim(), port)
        .parse()
        .map_err(|_| "Invalid address".to_string())?;
    let timeout = Duration::from_secs(2);

    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(err)) => {
            if err.kind() == std::io::ErrorKind::ConnectionRefused
                || err.kind() == std::io::ErrorKind::ConnectionReset
            {
                Ok(false)
            } else {
                Err(err.to_string())
            }
        }
        Err(_) => Ok(false), // Timeout
    }
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
pub fn profiles_update(
    app: AppHandle,
    state: State<AppState>,
    data: ProfilesData,
) -> Result<(), String> {
    if let Ok(mut guard) = state.profiles_pending.lock() {
        *guard = Some(data);
    }
    if state
        .profiles_saver_active
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let next = {
                    let state = handle.state::<AppState>();
                    let mut guard = state.profiles_pending.lock().unwrap();
                    guard.take()
                };
                if let Some(next_profiles) = next {
                    let paths = resolve_paths(&handle);
                    let _ = save_profiles_to(&next_profiles, &paths.profiles);
                    continue;
                }
                let state = handle.state::<AppState>();
                state.profiles_saver_active.store(false, Ordering::Relaxed);
                break;
            }
        });
    }
    Ok(())
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
pub fn queue_update(app: AppHandle, state: State<AppState>, data: QueueData) -> Result<(), String> {
    if let Ok(mut guard) = state.queue_pending.lock() {
        *guard = Some(data);
    }
    if state
        .queue_saver_active
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let next = {
                    let state = handle.state::<AppState>();
                    let mut guard = state.queue_pending.lock().unwrap();
                    guard.take()
                };
                if let Some(next_queue) = next {
                    let paths = resolve_paths(&handle);
                    let _ = save_queue_to(&next_queue, &paths.queue);
                    continue;
                }
                let state = handle.state::<AppState>();
                state.queue_saver_active.store(false, Ordering::Relaxed);
                break;
            }
        });
    }
    Ok(())
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

#[tauri::command]
pub fn set_save_logs(state: State<AppState>, enabled: bool) -> Result<(), String> {
    state.save_logs.store(enabled, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn set_ui_log_enabled(state: State<AppState>, enabled: bool) -> Result<(), String> {
    state
        .ui_log_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}
