use serde::Serialize;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use ps5upload_core::protocol::{
    get_payload_version, get_payload_status, queue_extract, queue_cancel, queue_clear,
    PayloadStatus,
};
use ps5upload_core::update::{download_asset, fetch_latest_release, fetch_release_by_tag};

const TRANSFER_PORT: u16 = 9113;
const PAYLOAD_PORT: u16 = 9021;

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
pub struct PayloadProbeResult {
    is_ps5upload: bool,
    message: String,
}

fn emit_log(handle: &AppHandle, message: impl Into<String>) {
    let _ = handle.emit(
        "payload_log",
        PayloadLogEvent {
            message: message.into(),
        },
    );
}

fn emit_done(handle: &AppHandle, bytes: Option<u64>, error: Option<String>) {
    let _ = handle.emit(
        "payload_done",
        PayloadDoneEvent { bytes, error },
    );
}

fn emit_version(handle: &AppHandle, version: Option<String>, error: Option<String>) {
    let _ = handle.emit(
        "payload_version",
        PayloadVersionEvent { version, error },
    );
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
    std::thread::spawn(move || {
        let (log_label, tmp_name, tag) = match fetch.as_str() {
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

        emit_log(&handle, log_label);
        let result = tauri::async_runtime::block_on(async {
            let release = if let Some(tag) = tag {
                match fetch_release_by_tag(&tag).await {
                    Ok(release) => release,
                    Err(_) => {
                        emit_log(
                            &handle,
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
            Ok::<_, String>(tmp_path)
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
