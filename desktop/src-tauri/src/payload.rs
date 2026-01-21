use serde::Serialize;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

use ps5upload_core::protocol::{
    get_payload_version, get_payload_status, queue_extract, queue_cancel, queue_clear,
    PayloadStatus,
};
use ps5upload_core::update::{download_asset, fetch_latest_release, fetch_release_by_tag};

use crate::logging::write_log_line;
use crate::state::{AppState, PayloadStatusCache};

const TRANSFER_PORT: u16 = 9113;
const PAYLOAD_PORT: u16 = 9021;
const PAYLOAD_POLL_INTERVAL_SECS: u64 = 5;
const PAYLOAD_POLL_ERROR_BACKOFF_SECS: u64 = 10;
const PAYLOAD_AUTO_RELOAD_INTERVAL_SECS: u64 = 60;

#[derive(Clone, Serialize)]
struct PayloadLogEvent {
    message: String,
}

#[derive(Clone, Serialize)]
struct PayloadDoneEvent {
    bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct PayloadVersionEvent {
    version: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct PayloadBusyEvent {
    busy: bool,
}

#[derive(Clone, Serialize)]
struct PayloadStatusUpdateEvent {
    status: Option<PayloadStatus>,
    error: Option<String>,
    updated_at_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct PayloadProbeResult {
    is_ps5upload: bool,
    message: String,
}

fn emit_log(handle: &AppHandle, message: impl Into<String>) {
    let message = message.into();
    let state = handle.state::<AppState>();
    if state.save_logs.load(std::sync::atomic::Ordering::Relaxed) {
        write_log_line(handle, "payload", &message);
    }
    if state.ui_log_enabled.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = handle.emit(
            "payload_log",
            PayloadLogEvent {
                message,
            },
        );
    }
}

fn emit_done(handle: &AppHandle, bytes: Option<u64>, error: Option<String>) {
    let _ = handle.emit(
        "payload_done",
        PayloadDoneEvent { bytes, error },
    );
}

fn emit_busy(handle: &AppHandle, busy: bool) {
    let _ = handle.emit("payload_busy", PayloadBusyEvent { busy });
}

fn emit_version(handle: &AppHandle, version: Option<String>, error: Option<String>) {
    let _ = handle.emit(
        "payload_version",
        PayloadVersionEvent { version, error },
    );
}

fn emit_status_update(handle: &AppHandle, cache: PayloadStatusCache) {
    let _ = handle.emit(
        "payload_status_update",
        PayloadStatusUpdateEvent {
            status: cache.status,
            error: cache.error,
            updated_at_ms: cache.updated_at_ms,
        },
    );
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn update_payload_cache(
    handle: &AppHandle,
    status: Option<PayloadStatus>,
    error: Option<String>,
) {
    let mut changed = false;
    let now = now_millis();
    let snapshot = {
        let state = handle.state::<AppState>();
        let mut guard = state.payload_status.lock().unwrap();
        if let Some(next_status) = status {
            if guard.status.as_ref() != Some(&next_status) {
                guard.status = Some(next_status);
                changed = true;
            }
        }
        if guard.error != error {
            guard.error = error;
            changed = true;
        }
        guard.updated_at_ms = now;
        guard.clone()
    };
    if changed {
        emit_status_update(handle, snapshot);
    }
}

async fn download_payload(fetch: &str, handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let (log_label, tmp_name, tag) = match fetch {
        "current" => {
            let tag = format!("v{}", include_str!("../../../VERSION").trim());
            (
                format!("Downloading payload {}...", tag),
                "ps5upload_current.elf".to_string(),
                Some(tag),
            )
        }
        _ => (
            "Downloading latest payload...".to_string(),
            "ps5upload_latest.elf".to_string(),
            None,
        ),
    };
    emit_log(handle, log_label);
    let release = if let Some(tag) = tag {
        match fetch_release_by_tag(&tag).await {
            Ok(release) => release,
            Err(_) => {
                emit_log(
                    handle,
                    format!("Tag {} not found, falling back to latest release.", tag),
                );
                fetch_latest_release(false)
                    .await
                    .map_err(|err| err.to_string())?
            }
        }
    } else {
        fetch_latest_release(false)
            .await
            .map_err(|err| err.to_string())?
    };
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == "ps5upload.elf")
        .ok_or_else(|| "Payload asset not found".to_string())?;
    let tmp_path = std::env::temp_dir().join(tmp_name);
    download_asset(&asset.browser_download_url, &tmp_path.display().to_string())
        .await
        .map_err(|err| err.to_string())?;
    Ok(tmp_path)
}

pub fn start_payload_auto_reloader(app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last_run: Option<std::time::Instant> = None;
        loop {
            let state = app_handle.state::<AppState>();
            if !state.payload_auto_reload_enabled.load(Ordering::Relaxed) {
                last_run = None;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if state.transfer_active.load(Ordering::Relaxed)
                || state.payload_auto_reload_inflight.load(Ordering::Relaxed)
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            let ip = {
                state
                    .payload_ip
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default()
            };
            if ip.trim().is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if let Some(last) = last_run {
                if last.elapsed() < Duration::from_secs(PAYLOAD_AUTO_RELOAD_INTERVAL_SECS) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            }
            state
                .payload_auto_reload_inflight
                .store(true, Ordering::Relaxed);
            emit_busy(&app_handle, true);
            let mode = {
                state
                    .payload_auto_reload_mode
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_else(|_| "current".to_string())
            };
            let local_path = {
                state
                    .payload_auto_reload_path
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default()
            };
            let result = match mode.as_str() {
                "local" => {
                    if local_path.trim().is_empty() {
                        Err("Select a payload (.elf/.bin) file first.".to_string())
                    } else {
                        let handle = app_handle.clone();
                        let ip_clone = ip.clone();
                        let path_clone = local_path.clone();
                        tokio::task::spawn_blocking(move || {
                            emit_log(&handle, format!("Auto-reload payload from {}", path_clone));
                            send_payload_file(&ip_clone, &path_clone, &handle)
                        })
                        .await
                        .unwrap_or_else(|_| Err("Auto-reload thread failed.".to_string()))
                    }
                }
                "current" | "latest" => {
                    let handle = app_handle.clone();
                    let ip_clone = ip.clone();
                    let fetch = mode.clone();
                    match download_payload(&fetch, &handle).await {
                        Ok(path) => {
                            let path_str = path.display().to_string();
                            tokio::task::spawn_blocking(move || {
                                emit_log(&handle, format!("Auto-reload payload from {}", path_str));
                                send_payload_file(&ip_clone, &path_str, &handle)
                            })
                            .await
                            .unwrap_or_else(|_| Err("Auto-reload thread failed.".to_string()))
                        }
                        Err(err) => Err(err),
                    }
                }
                _ => Err("Invalid payload reload mode.".to_string()),
            };
            match result {
                Ok(bytes) => {
                    emit_log(&app_handle, "Auto-reload payload sent.");
                    emit_done(&app_handle, Some(bytes), None);
                }
                Err(err) => {
                    emit_log(&app_handle, format!("Auto-reload failed: {}", err));
                    emit_done(&app_handle, None, Some(err));
                }
            }
            emit_busy(&app_handle, false);
            state
                .payload_auto_reload_inflight
                .store(false, Ordering::Relaxed);
            last_run = Some(std::time::Instant::now());
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

pub fn start_payload_poller(app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut backoff_until: Option<std::time::Instant> = None;
        loop {
            let state = app_handle.state::<AppState>();
            if !state.payload_poll_enabled.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            let ip = {
                state
                    .payload_ip
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default()
            };
            if ip.trim().is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if let Some(until) = backoff_until {
                if std::time::Instant::now() < until {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            }
            match get_payload_status(&ip, TRANSFER_PORT).await {
                Ok(status) => {
                    backoff_until = None;
                    update_payload_cache(&app_handle, Some(status), None);
                }
                Err(err) => {
                    backoff_until =
                        Some(std::time::Instant::now() + Duration::from_secs(PAYLOAD_POLL_ERROR_BACKOFF_SECS));
                    update_payload_cache(&app_handle, None, Some(err.to_string()));
                }
            }
            tokio::time::sleep(Duration::from_secs(PAYLOAD_POLL_INTERVAL_SECS)).await;
        }
    });
}

fn payload_path_is_elf(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let e = ext.to_lowercase();
            e == "elf" || e == "bin"
        })
        .unwrap_or(false)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn probe_payload_file(path: &str) -> Result<PayloadProbeResult, String> {
    if !payload_path_is_elf(path) {
        return Ok(PayloadProbeResult {
            is_ps5upload: false,
            message: "Payload must be a .elf or .bin file.".to_string(),
        });
    }
    let name_match = path.to_lowercase().contains("ps5upload");
    let mut file = File::open(path).map_err(|e| format!("Failed to open payload: {}", e))?;
    let mut buffer = vec![0u8; 512 * 1024];
    let read_len = file
        .read(&mut buffer)
        .map_err(|e| format!("Failed to read payload: {}", e))?;
    let content = &buffer[..read_len];
    let signature_match = contains_bytes(content, b"ps5upload")
        || contains_bytes(content, b"PS5UPLOAD");
    if name_match || signature_match {
        Ok(PayloadProbeResult {
            is_ps5upload: true,
            message: "PS5Upload payload detected.".to_string(),
        })
    } else {
        Ok(PayloadProbeResult {
            is_ps5upload: false,
            message: "No PS5Upload signature found. Use only if you trust this payload.".to_string(),
        })
    }
}

fn send_payload_file(ip: &str, path: &str, handle: &AppHandle) -> Result<u64, String> {
    if !payload_path_is_elf(path) {
        return Err("Payload must be a .elf or .bin file.".to_string());
    }

    let mut file = File::open(path).map_err(|e| format!("Failed to open payload: {}", e))?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file_len > 0 {
        emit_log(handle, format!("Payload size: {} bytes", file_len));
    }
    let mut stream =
        TcpStream::connect((ip, PAYLOAD_PORT)).map_err(|e| format!("Failed to connect: {}", e))?;
    let _ = stream.set_nodelay(true);
    let mut buffer = vec![0u8; 256 * 1024];
    let mut sent = 0u64;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("Send failed: {}", e))?;
        if n == 0 {
            break;
        }
        stream
            .write_all(&buffer[..n])
            .map_err(|e| format!("Send failed: {}", e))?;
        sent += n as u64;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);

    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut discard = [0u8; 1024];
    while match stream.read(&mut discard) {
        Ok(n) => n > 0,
        Err(_) => false,
    } {}

    if file_len > 0 && sent != file_len {
        return Err(format!("Send incomplete: {} of {} bytes", sent, file_len));
    }
    Ok(sent)
}

#[tauri::command]
pub fn payload_send(ip: String, path: String, app_handle: AppHandle) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() {
        return Err("Select a payload (.elf/.bin) file first.".to_string());
    }
    if !payload_path_is_elf(&path) {
        return Err("Payload must be a .elf or .bin file.".to_string());
    }

    let handle = app_handle.clone();
    emit_busy(&handle, true);
    std::thread::spawn(move || {
        emit_log(&handle, format!("Sending payload to {}:{}...", ip, PAYLOAD_PORT));
        emit_log(&handle, format!("Payload path: {}", path));
        let result = send_payload_file(&ip, &path, &handle);
        match result {
            Ok(bytes) => {
                emit_log(&handle, "Payload sent successfully.");
                emit_done(&handle, Some(bytes), None);
            }
            Err(err) => {
                emit_log(&handle, format!("Payload failed: {}", err));
                emit_done(&handle, None, Some(err));
            }
        }
        emit_busy(&handle, false);
    });

    Ok(())
}

#[tauri::command]
pub fn payload_download_and_send(
    ip: String,
    fetch: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }

    let handle = app_handle.clone();
    emit_busy(&handle, true);
    std::thread::spawn(move || {
        let result = tauri::async_runtime::block_on(async {
            let path = download_payload(&fetch, &handle).await?;
            Ok::<_, String>(path)
        });

        match result {
            Ok(path) => {
                let path_str = path.display().to_string();
                emit_log(&handle, format!("Payload downloaded: {}", path_str));
                let result = send_payload_file(&ip, &path_str, &handle);
                match result {
                    Ok(bytes) => emit_done(&handle, Some(bytes), None),
                    Err(err) => emit_done(&handle, None, Some(err)),
                }
            }
            Err(err) => emit_done(&handle, None, Some(err)),
        }
        emit_busy(&handle, false);
    });

    Ok(())
}

#[tauri::command]
pub fn payload_check(ip: String, app_handle: AppHandle) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }

    let handle = app_handle.clone();
    std::thread::spawn(move || {
        let res = tauri::async_runtime::block_on(async {
            get_payload_version(&ip, TRANSFER_PORT)
                .await
                .map_err(|err| err.to_string())
        });
        match res {
            Ok(version) => emit_version(&handle, Some(version), None),
            Err(err) => emit_version(&handle, None, Some(err)),
        }
    });

    Ok(())
}

#[tauri::command]
pub fn payload_probe(path: String) -> Result<PayloadProbeResult, String> {
    if path.trim().is_empty() {
        return Err("Select a payload (.elf/.bin) file first.".to_string());
    }
    probe_payload_file(&path)
}

#[tauri::command]
pub async fn payload_status(ip: String) -> Result<PayloadStatus, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }

    get_payload_status(&ip, TRANSFER_PORT)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn payload_status_snapshot(state: State<AppState>) -> PayloadStatusCache {
    state
        .payload_status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn payload_status_refresh(
    ip: String,
    app_handle: AppHandle,
) -> Result<PayloadStatusCache, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    match get_payload_status(&ip, TRANSFER_PORT).await {
        Ok(status) => {
            update_payload_cache(&app_handle, Some(status), None);
        }
        Err(err) => {
            update_payload_cache(&app_handle, None, Some(err.to_string()));
        }
    }
    let snapshot = app_handle
        .state::<AppState>()
        .payload_status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    Ok(snapshot)
}

#[tauri::command]
pub fn payload_polling_set(state: State<AppState>, enabled: bool) -> Result<(), String> {
    state.payload_poll_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn payload_set_ip(state: State<AppState>, ip: String) -> Result<(), String> {
    if let Ok(mut guard) = state.payload_ip.lock() {
        *guard = ip.trim().to_string();
    }
    Ok(())
}

#[tauri::command]
pub fn payload_auto_reload_set(
    state: State<AppState>,
    enabled: bool,
    mode: String,
    local_path: String,
) -> Result<(), String> {
    state
        .payload_auto_reload_enabled
        .store(enabled, Ordering::Relaxed);
    if let Ok(mut guard) = state.payload_auto_reload_mode.lock() {
        *guard = mode;
    }
    if let Ok(mut guard) = state.payload_auto_reload_path.lock() {
        *guard = local_path;
    }
    Ok(())
}

#[tauri::command]
pub fn payload_queue_extract(ip: String, src: String, dst: String) -> Result<i32, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if src.trim().is_empty() {
        return Err("Source path is required.".to_string());
    }
    if dst.trim().is_empty() {
        return Err("Destination path is required.".to_string());
    }

    tauri::async_runtime::block_on(async {
        queue_extract(&ip, TRANSFER_PORT, &src, &dst)
            .await
            .map_err(|err| err.to_string())
    })
}

#[tauri::command]
pub fn payload_queue_cancel(ip: String, id: i32) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }

    tauri::async_runtime::block_on(async {
        queue_cancel(&ip, TRANSFER_PORT, id)
            .await
            .map_err(|err| err.to_string())
    })
}

#[tauri::command]
pub fn payload_queue_clear(ip: String) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }

    tauri::async_runtime::block_on(async {
        queue_clear(&ip, TRANSFER_PORT)
            .await
            .map_err(|err| err.to_string())
    })
}
