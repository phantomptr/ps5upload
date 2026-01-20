use serde::Serialize;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use tauri::{AppHandle, Emitter, State};

use ps5upload_core::protocol::{
    chmod_777, copy_path_with_progress, create_path, delete_path, download_dir_with_progress,
    download_file_with_progress, extract_archive_with_progress, move_path, move_path_with_progress,
    upload_v2_init, DownloadCompression, DirEntry,
};
use ps5upload_core::transfer::{collect_files_with_progress, send_files_v2_for_list, CompressionMode, FileEntry, SendFilesConfig};
use ps5upload_core::transfer_utils::{parse_upload_response, read_upload_response};

use crate::state::AppState;

const TRANSFER_PORT: u16 = 9113;

#[derive(Clone, Serialize)]
struct ManageProgressEvent {
    op: String,
    processed: u64,
    total: u64,
    current_file: Option<String>,
}

#[derive(Clone, Serialize)]
struct ManageDoneEvent {
    op: String,
    bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct ManageLogEvent {
    message: String,
}

fn emit_progress(
    handle: &AppHandle,
    op: &str,
    processed: u64,
    total: u64,
    current_file: Option<String>,
) {
    let _ = handle.emit(
        "manage_progress",
        ManageProgressEvent {
            op: op.to_string(),
            processed,
            total,
            current_file,
        },
    );
}

fn emit_done(handle: &AppHandle, op: &str, bytes: Option<u64>, error: Option<String>) {
    let _ = handle.emit(
        "manage_done",
        ManageDoneEvent {
            op: op.to_string(),
            bytes,
            error,
        },
    );
}

fn emit_log(handle: &AppHandle, message: impl Into<String>) {
    let _ = handle.emit(
        "manage_log",
        ManageLogEvent {
            message: message.into(),
        },
    );
}

fn join_remote_path(base: &str, name: &str) -> String {
    if base.trim().is_empty() {
        return format!("/{}", name.trim_start_matches('/'));
    }
    if base == "/" {
        return format!("/{}", name.trim_start_matches('/'));
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        name.trim_start_matches('/')
    )
}

fn ensure_manage_idle(state: &State<AppState>) -> Result<(), String> {
    if state.manage_active.swap(true, Ordering::Relaxed) {
        return Err("Another manage operation is already running".to_string());
    }
    state.manage_cancel.store(false, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn manage_cancel(state: State<AppState>) -> Result<(), String> {
    state.manage_cancel.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn manage_list(ip: String, path: String) -> Result<Vec<DirEntry>, String> {
    tauri::async_runtime::block_on(async {
        ps5upload_core::protocol::list_dir(&ip, TRANSFER_PORT, &path)
            .await
            .map_err(|err| err.to_string())
    })
}

#[tauri::command]
pub fn manage_delete(ip: String, path: String, app_handle: AppHandle, state: State<AppState>) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() {
        return Err("Select a path to delete.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Delete {}", path));
        let result = tauri::async_runtime::block_on(async {
            delete_path(&ip, TRANSFER_PORT, &path)
                .await
                .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Delete", None, None),
            Err(err) => emit_done(&handle, "Delete", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_rename(
    ip: String,
    src_path: String,
    dst_path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if src_path.trim().is_empty() || dst_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Rename {} -> {}", src_path, dst_path));
        let result = tauri::async_runtime::block_on(async {
            move_path(&ip, TRANSFER_PORT, &src_path, &dst_path)
                .await
                .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Rename", None, None),
            Err(err) => emit_done(&handle, "Rename", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_create_dir(
    ip: String,
    path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() {
        return Err("Folder path is required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Create folder {}", path));
        let result = tauri::async_runtime::block_on(async {
            create_path(&ip, TRANSFER_PORT, &path)
                .await
                .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Create", None, None),
            Err(err) => emit_done(&handle, "Create", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_chmod(
    ip: String,
    path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() {
        return Err("Select a path.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("chmod 777 {}", path));
        let result = tauri::async_runtime::block_on(async {
            chmod_777(&ip, TRANSFER_PORT, &path)
                .await
                .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "chmod", None, None),
            Err(err) => emit_done(&handle, "chmod", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_move(
    ip: String,
    src_path: String,
    dst_path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if src_path.trim().is_empty() || dst_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Move {} -> {}", src_path, dst_path));
        let handle_progress = handle.clone();
        let result = tauri::async_runtime::block_on(async move {
            move_path_with_progress(
                &ip,
                TRANSFER_PORT,
                &src_path,
                &dst_path,
                cancel,
                move |processed, total| {
                    emit_progress(&handle_progress, "Move", processed, total, None);
                },
            )
            .await
            .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Move", None, None),
            Err(err) => emit_done(&handle, "Move", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_copy(
    ip: String,
    src_path: String,
    dst_path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if src_path.trim().is_empty() || dst_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Copy {} -> {}", src_path, dst_path));
        let handle_progress = handle.clone();
        let result = tauri::async_runtime::block_on(async move {
            copy_path_with_progress(
                &ip,
                TRANSFER_PORT,
                &src_path,
                &dst_path,
                cancel,
                move |processed, total| {
                    emit_progress(&handle_progress, "Copy", processed, total, None);
                },
            )
            .await
            .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Copy", None, None),
            Err(err) => emit_done(&handle, "Copy", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_extract(
    ip: String,
    src_path: String,
    dst_path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if src_path.trim().is_empty() || dst_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Extract {} -> {}", src_path, dst_path));
        let handle_progress = handle.clone();
        let result = tauri::async_runtime::block_on(async move {
            extract_archive_with_progress(
                &ip,
                TRANSFER_PORT,
                &src_path,
                &dst_path,
                cancel,
                move |processed, total| {
                    emit_progress(&handle_progress, "Extract", processed, total, None);
                },
            )
            .await
            .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => emit_done(&handle, "Extract", None, None),
            Err(err) => emit_done(&handle, "Extract", None, Some(err)),
        }
    });
    Ok(())
}

fn parse_download_compression(value: &str) -> DownloadCompression {
    match value.to_lowercase().as_str() {
        "lz4" => DownloadCompression::Lz4,
        "zstd" => DownloadCompression::Zstd,
        "lzma" => DownloadCompression::Lzma,
        "auto" => DownloadCompression::Auto,
        _ => DownloadCompression::None,
    }
}

#[tauri::command]
pub fn manage_download_file(
    ip: String,
    path: String,
    dest_path: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() || dest_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    thread::spawn(move || {
        emit_log(&handle, format!("Download {}", path));
        let handle_progress = handle.clone();
        let result = tauri::async_runtime::block_on(async move {
            download_file_with_progress(
                &ip,
                TRANSFER_PORT,
                &path,
                &dest_path,
                cancel,
                move |received, total, current| {
                    emit_progress(&handle_progress, "Download", received, total, current);
                },
            )
            .await
            .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(bytes) => emit_done(&handle, "Download", Some(bytes), None),
            Err(err) => emit_done(&handle, "Download", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_download_dir(
    ip: String,
    path: String,
    dest_path: String,
    compression: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() || dest_path.trim().is_empty() {
        return Err("Source and destination are required.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    let comp = parse_download_compression(&compression);
    thread::spawn(move || {
        emit_log(&handle, format!("Download {}", path));
        let handle_progress = handle.clone();
        let handle_info = handle.clone();
        let result = tauri::async_runtime::block_on(async move {
            download_dir_with_progress(
                &ip,
                TRANSFER_PORT,
                &path,
                &dest_path,
                cancel,
                comp,
                move |received, total, current| {
                    emit_progress(&handle_progress, "Download", received, total, current);
                },
                move |info| {
                    if let Some(comp) = info {
                        emit_log(&handle_info, format!("Compression used: {}", comp));
                    }
                },
            )
            .await
            .map_err(|err| err.to_string())
        });
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(bytes) => emit_done(&handle, "Download", Some(bytes), None),
            Err(err) => emit_done(&handle, "Download", None, Some(err)),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn manage_upload(
    ip: String,
    dest_root: String,
    paths: Vec<String>,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if dest_root.trim().is_empty() {
        return Err("Destination path is required.".to_string());
    }
    if paths.is_empty() {
        return Err("Select at least one file or folder.".to_string());
    }
    ensure_manage_idle(&state)?;

    let handle = app_handle.clone();
    let active = state.manage_active.clone();
    let cancel = state.manage_cancel.clone();
    thread::spawn(move || {
        emit_log(&handle, "Upload started.");
        let result = manage_upload_impl(&ip, &dest_root, paths, handle.clone(), cancel);
        active.store(false, Ordering::Relaxed);
        match result {
            Ok(bytes) => emit_done(&handle, "Upload", Some(bytes), None),
            Err(err) => emit_done(&handle, "Upload", None, Some(err)),
        }
    });
    Ok(())
}

fn manage_upload_impl(
    ip: &str,
    dest_root: &str,
    paths: Vec<String>,
    handle: AppHandle,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<u64, String> {
    let mut batches: Vec<(String, Vec<FileEntry>, u64)> = Vec::new();
    let mut total_bytes = 0u64;

    for path in paths {
        if cancel.load(Ordering::Relaxed) {
            return Err("Upload cancelled".to_string());
        }
        let path_buf = Path::new(&path);
        if path_buf.is_dir() {
            let folder_name = path_buf
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("folder");
            let dest = join_remote_path(dest_root, folder_name);
            let (files, cancelled) = collect_files_with_progress(&path, cancel.clone(), |_, _| {});
            if cancelled {
                return Err("Upload cancelled".to_string());
            }
            if files.is_empty() {
                continue;
            }
            let batch_bytes: u64 = files.iter().map(|f| f.size).sum();
            total_bytes = total_bytes.saturating_add(batch_bytes);
            batches.push((dest, files, batch_bytes));
        } else if path_buf.is_file() {
            let meta = std::fs::metadata(path_buf).map_err(|err| err.to_string())?;
            let rel_path = path_buf
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            let entry = FileEntry {
                rel_path,
                abs_path: path_buf.to_path_buf(),
                size: meta.len(),
                mtime: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
            };
            let entry_size = entry.size;
            total_bytes = total_bytes.saturating_add(entry_size);
            batches.push((dest_root.to_string(), vec![entry], entry_size));
        }
    }

    if batches.is_empty() {
        return Err("No files to upload.".to_string());
    }

    emit_progress(&handle, "Upload", 0, total_bytes, None);

    let mut sent_offset = 0u64;
    for (dest, files, batch_bytes) in batches {
        if cancel.load(Ordering::Relaxed) {
            return Err("Upload cancelled".to_string());
        }
        let stream = tauri::async_runtime::block_on(async {
            upload_v2_init(ip, TRANSFER_PORT, &dest, false).await
        })
        .map_err(|err| err.to_string())?;
        let mut std_stream = stream.into_std().map_err(|err| err.to_string())?;
        std_stream
            .set_nonblocking(true)
            .map_err(|err| err.to_string())?;

        let mut last_sent = 0u64;
        let base_offset = sent_offset;
        let total_bytes_copy = total_bytes;
        let handle_progress = handle.clone();
        send_files_v2_for_list(
            files,
            std_stream.try_clone().map_err(|err| err.to_string())?,
            SendFilesConfig {
                cancel: cancel.clone(),
                progress: move |sent, _files, _current| {
                    if sent == last_sent {
                        return;
                    }
                    emit_progress(
                        &handle_progress,
                        "Upload",
                        base_offset + sent,
                        total_bytes_copy,
                        None,
                    );
                    last_sent = sent;
                },
                log: |_| {},
                worker_id: 0,
                allowed_connections: None,
                compression: CompressionMode::None,
                rate_limit_bps: None,
            },
        )
        .map_err(|err| err.to_string())?;

        let response =
            read_upload_response(&mut std_stream, &cancel).map_err(|err| err.to_string())?;
        parse_upload_response(&response).map_err(|err| err.to_string())?;

        sent_offset = sent_offset.saturating_add(batch_bytes);
        emit_progress(&handle, "Upload", sent_offset, total_bytes, None);
    }

    Ok(total_bytes)
}
