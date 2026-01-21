use serde::Serialize;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

use ps5upload_core::protocol::{list_storage, StorageLocation};

use crate::state::{AppState, ConnectionStatusCache};

const TRANSFER_PORT: u16 = 9113;
const CONNECT_POLL_INTERVAL_SECS: u64 = 5;

#[derive(Clone, Serialize)]
struct ConnectionStatusUpdateEvent {
    is_connected: bool,
    status: String,
    storage_locations: Vec<StorageLocation>,
}

fn emit_status_update(handle: &AppHandle, cache: &ConnectionStatusCache) {
    let _ = handle.emit(
        "connection_status_update",
        ConnectionStatusUpdateEvent {
            is_connected: cache.is_connected,
            status: cache.status.clone(),
            storage_locations: cache.storage_locations.clone(),
        },
    );
}

fn update_connection_cache(handle: &AppHandle, next: ConnectionStatusCache) -> ConnectionStatusCache {
    let mut changed = false;
    let snapshot = {
        let state = handle.state::<AppState>();
        let mut guard = state.connection_status.lock().unwrap();
        if guard.is_connected != next.is_connected
            || guard.status != next.status
            || guard.storage_locations != next.storage_locations
        {
            *guard = next;
            changed = true;
        }
        guard.clone()
    };
    if changed {
        emit_status_update(handle, &snapshot);
    }
    snapshot
}

async fn check_port(ip: &str, port: u16) -> Result<bool, String> {
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
        Err(_) => Ok(false),
    }
}

async fn connect_once(ip: &str) -> Result<ConnectionStatusCache, String> {
    if ip.trim().is_empty() {
        return Ok(ConnectionStatusCache {
            is_connected: false,
            status: "Missing IP".to_string(),
            storage_locations: Vec::new(),
        });
    }
    let port_open = check_port(ip, TRANSFER_PORT).await?;
    if !port_open {
        return Ok(ConnectionStatusCache {
            is_connected: false,
            status: format!("Port {} closed", TRANSFER_PORT),
            storage_locations: Vec::new(),
        });
    }
    let storage = list_storage(ip, TRANSFER_PORT)
        .await
        .map_err(|err| err.to_string())?;
    let available: Vec<StorageLocation> = storage
        .into_iter()
        .filter(|loc| loc.free_gb > 0.0)
        .collect();
    if available.is_empty() {
        Ok(ConnectionStatusCache {
            is_connected: false,
            status: "No storage".to_string(),
            storage_locations: Vec::new(),
        })
    } else {
        Ok(ConnectionStatusCache {
            is_connected: true,
            status: "Connected".to_string(),
            storage_locations: available,
        })
    }
}

pub fn start_connection_poller(app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let state = app_handle.state::<AppState>();
            if !state.connection_poll_enabled.load(Ordering::Relaxed)
                || !state.connection_auto_enabled.load(Ordering::Relaxed)
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            let ip = if let Ok(guard) = state.connection_ip.lock() {
                guard.clone()
            } else {
                String::new()
            };
            if ip.trim().is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            match connect_once(&ip).await {
                Ok(snapshot) => {
                    update_connection_cache(&app_handle, snapshot);
                }
                Err(err) => {
                    update_connection_cache(
                        &app_handle,
                        ConnectionStatusCache {
                            is_connected: false,
                            status: format!("Error: {}", err),
                            storage_locations: Vec::new(),
                        },
                    );
                }
            }
            tokio::time::sleep(Duration::from_secs(CONNECT_POLL_INTERVAL_SECS)).await;
        }
    });
}

#[tauri::command]
pub fn connection_set_ip(state: State<AppState>, ip: String) -> Result<(), String> {
    if let Ok(mut guard) = state.connection_ip.lock() {
        *guard = ip.trim().to_string();
    }
    Ok(())
}

#[tauri::command]
pub fn connection_polling_set(state: State<AppState>, enabled: bool) -> Result<(), String> {
    state.connection_poll_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn connection_auto_set(state: State<AppState>, enabled: bool) -> Result<(), String> {
    state.connection_auto_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn connection_snapshot(state: State<AppState>) -> ConnectionStatusCache {
    state
        .connection_status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn connection_connect(ip: String, app_handle: AppHandle) -> Result<ConnectionStatusCache, String> {
    let snapshot = connect_once(&ip).await?;
    Ok(update_connection_cache(&app_handle, snapshot))
}
