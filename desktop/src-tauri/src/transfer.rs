use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use ps5upload_core::protocol::{
    check_dir, get_space, hash_file, list_dir_recursive, upload_rar_for_extraction, upload_v2_init,
    RarExtractMode,
};
use ps5upload_core::transfer::{
    collect_files_with_progress, scan_7z_archive, scan_zip_archive, send_7z_archive,
    send_files_v2_for_list, send_zip_archive, stream_files_with_progress, CompressionMode,
    SendFilesConfig, SharedReceiverIterator,
};
use ps5upload_core::transfer_utils::{
    choose_best_compression, compression_label, parse_upload_response, partition_files_by_size,
    payload_supports_modern_compression, read_upload_response, recommend_connections,
    sample_bytes_from_files, sample_bytes_from_path, sample_workload,
};

use crate::state::AppState;

const TRANSFER_PORT: u16 = 9113;
const MAX_PARALLEL_CONNECTIONS: usize = 10;

#[derive(Debug, Deserialize)]
pub struct TransferRequest {
    pub ip: String,
    pub source_path: String,
    pub dest_path: String,
    pub use_temp: bool,
    pub connections: usize,
    pub resume_mode: String,
    pub compression: String,
    pub bandwidth_limit_mbps: f64,
    pub auto_tune_connections: bool,
    pub optimize_upload: bool,
    pub rar_extract_mode: String,
    pub payload_version: Option<String>,
    pub storage_root: Option<String>,
    pub required_size: Option<u64>,
}

#[derive(Clone, Serialize)]
struct TransferScanEvent {
    run_id: u64,
    files_found: usize,
    total_size: u64,
}

#[derive(Clone, Serialize)]
struct TransferProgressEvent {
    run_id: u64,
    sent: u64,
    total: u64,
    files_sent: i32,
    elapsed_secs: f64,
    current_file: Option<String>,
}

#[derive(Clone, Serialize)]
struct TransferCompleteEvent {
    run_id: u64,
    files: i32,
    bytes: u64,
}

#[derive(Clone, Serialize)]
struct TransferErrorEvent {
    run_id: u64,
    message: String,
}

#[derive(Clone, Serialize)]
struct TransferLogEvent {
    run_id: u64,
    message: String,
}

fn emit_log(handle: &AppHandle, run_id: u64, message: impl Into<String>) {
    let _ = handle.emit(
        "transfer_log",
        TransferLogEvent {
            run_id,
            message: message.into(),
        },
    );
}

fn emit_scan(handle: &AppHandle, run_id: u64, files_found: usize, total_size: u64) {
    let _ = handle.emit(
        "transfer_scan",
        TransferScanEvent {
            run_id,
            files_found,
            total_size,
        },
    );
}

fn emit_progress(
    handle: &AppHandle,
    run_id: u64,
    sent: u64,
    total: u64,
    files_sent: i32,
    elapsed_secs: f64,
    current_file: Option<String>,
) {
    let _ = handle.emit(
        "transfer_progress",
        TransferProgressEvent {
            run_id,
            sent,
            total,
            files_sent,
            elapsed_secs,
            current_file,
        },
    );
}

fn emit_complete(handle: &AppHandle, run_id: u64, files: i32, bytes: u64) {
    let _ = handle.emit(
        "transfer_complete",
        TransferCompleteEvent { run_id, files, bytes },
    );
}

fn emit_error(handle: &AppHandle, run_id: u64, message: impl Into<String>) {
    let _ = handle.emit(
        "transfer_error",
        TransferErrorEvent {
            run_id,
            message: message.into(),
        },
    );
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn parse_rar_mode(mode: &str) -> RarExtractMode {
    match mode.to_lowercase().as_str() {
        "safe" => RarExtractMode::Safe,
        "turbo" => RarExtractMode::Turbo,
        _ => RarExtractMode::Normal,
    }
}

fn parse_compression_mode(mode: &str) -> CompressionMode {
    match mode.to_lowercase().as_str() {
        "lz4" => CompressionMode::Lz4,
        "zstd" => CompressionMode::Zstd,
        "lzma" => CompressionMode::Lzma,
        _ => CompressionMode::None,
    }
}

#[tauri::command]
pub async fn transfer_check_dest(ip: String, dest_path: String) -> Result<bool, String> {
    check_dir(&ip, TRANSFER_PORT, &dest_path)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn transfer_scan(
    source_path: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    let run_id = state.transfer_run_id.fetch_add(1, Ordering::Relaxed) + 1;
    let cancel = state.transfer_cancel.clone();
    cancel.store(false, Ordering::Relaxed);

    tauri::async_runtime::spawn_blocking(move || {
        let (files, _) = collect_files_with_progress(&source_path, cancel, |files_found, total| {
            emit_scan(&app_handle, run_id, files_found, total);
        });
        let total_size: u64 = files.iter().map(|f| f.size).sum();
        emit_scan(&app_handle, run_id, files.len(), total_size);
    });

    Ok(run_id)
}

#[tauri::command]
pub fn transfer_cancel(state: State<AppState>) -> Result<(), String> {
    state.transfer_cancel.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn transfer_start(
    req: TransferRequest,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    if req.ip.trim().is_empty() {
        return Err("PS5 IP address is required".to_string());
    }
    if req.source_path.trim().is_empty() {
        return Err("Source path is required".to_string());
    }
    if req.dest_path.trim().is_empty() {
        return Err("Destination path is required".to_string());
    }

    if state.transfer_active.load(Ordering::Relaxed) {
        return Err("Transfer already running".to_string());
    }

    let run_id = state.transfer_run_id.fetch_add(1, Ordering::Relaxed) + 1;
    let cancel = state.transfer_cancel.clone();
    let active = state.transfer_active.clone();
    cancel.store(false, Ordering::Relaxed);
    active.store(true, Ordering::Relaxed);

    tauri::async_runtime::spawn_blocking(move || {
        let result = tauri::async_runtime::block_on(run_transfer(
            req,
            run_id,
            &app_handle,
            cancel.clone(),
        ));
        active.store(false, Ordering::Relaxed);
        match result {
            Ok((files, bytes)) => emit_complete(&app_handle, run_id, files, bytes),
            Err(err) => emit_error(&app_handle, run_id, err),
        }
    });

    Ok(run_id)
}

async fn run_transfer(
    mut req: TransferRequest,
    run_id: u64,
    handle: &AppHandle,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(i32, u64), String> {
    let ip = req.ip.clone();
    let source_path = req.source_path.clone();
    let dest_path = req.dest_path.clone();
    let use_temp = req.use_temp;
    let bandwidth_limit_bps = (req.bandwidth_limit_mbps * 1024.0 * 1024.0) as u64;
    let payload_modern = payload_supports_modern_compression(req.payload_version.as_deref());

    if let (Some(required), Some(storage_root)) = (req.required_size, req.storage_root.as_ref()) {
        let required_safe = required.saturating_add(64 * 1024 * 1024);
        let space_result = get_space(&ip, TRANSFER_PORT, storage_root).await;
        if let Ok((free_bytes, _)) = space_result {
            if free_bytes < required_safe {
                let msg = format!(
                    "Insufficient space: {} free, {} required",
                    format_bytes(free_bytes),
                    format_bytes(required_safe)
                );
                emit_log(handle, run_id, msg.clone());
                return Err("Not enough free space on target drive".to_string());
            }
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    let path_low = source_path.to_lowercase();
    let is_rar = path_low.ends_with(".rar");
    let is_zip = path_low.ends_with(".zip");
    let is_7z = path_low.ends_with(".7z");
    let is_archive = is_rar || is_zip || is_7z;
    if is_archive && req.resume_mode != "none" {
        req.resume_mode = "none".to_string();
        emit_log(handle, run_id, "Resume is disabled for archive uploads.".to_string());
    }

    if is_rar {
        emit_log(handle, run_id, "Uploading RAR to PS5 for extraction...".to_string());
        let start = std::time::Instant::now();
        let tx_handle = handle.clone();
        let progress = move |sent: u64, total: u64| {
            let elapsed = start.elapsed().as_secs_f64();
            emit_progress(&tx_handle, run_id, sent, total, 0, elapsed, Some("Uploading RAR...".to_string()));
        };
        let log_handle = handle.clone();
        let extract_log = move |msg: String| emit_log(&log_handle, run_id, msg);
        let mode = parse_rar_mode(&req.rar_extract_mode);
        let result = upload_rar_for_extraction(
                &ip,
                TRANSFER_PORT,
                &source_path,
                &dest_path,
                mode,
                cancel.clone(),
                progress,
                extract_log,
            )
            .await;
        return result
            .map(|(files, bytes)| (files as i32, bytes))
            .map_err(|err| err.to_string());
    }

    if is_zip || is_7z {
        let source_path_clone = source_path.clone();
        let (count, size) = tauri::async_runtime::spawn_blocking(move || {
            if is_zip {
                scan_zip_archive(&source_path_clone)
            } else {
                scan_7z_archive(&source_path_clone)
            }
        }).await.map_err(|e| e.to_string())?.map_err(|err| err.to_string())?;

        emit_scan(handle, run_id, count, size);

        let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, use_temp).await
            .map_err(|err| err.to_string())?;
        let mut std_stream = stream.into_std().map_err(|err| err.to_string())?;
        std_stream
            .set_nonblocking(true)
            .map_err(|err| err.to_string())?;

        let start = std::time::Instant::now();
        let mut last_sent = 0u64;
        let rate_limit = if bandwidth_limit_bps > 0 {
            Some(bandwidth_limit_bps)
        } else {
            None
        };

        let tx_handle = handle.clone();
        let progress = move |sent: u64, files_sent: i32, current_file: Option<String>| {
            if sent == last_sent {
                return;
            }
            let elapsed = start.elapsed().as_secs_f64();
            emit_progress(&tx_handle, run_id, sent, size, files_sent, elapsed, current_file);
            last_sent = sent;
        };
        let log_handle = handle.clone();
        let log = move |msg: String| emit_log(&log_handle, run_id, msg);
        
        let source_path_clone = source_path.clone();
        let stream_clone = std_stream.try_clone().map_err(|err| err.to_string())?;
        let cancel_clone = cancel.clone();

        tauri::async_runtime::spawn_blocking(move || {
            if is_zip {
                send_zip_archive(
                    source_path_clone,
                    stream_clone,
                    cancel_clone,
                    progress,
                    log,
                    rate_limit,
                )
            } else {
                send_7z_archive(
                    source_path_clone,
                    stream_clone,
                    cancel_clone,
                    progress,
                    log,
                    rate_limit,
                )
            }
        }).await.map_err(|e| e.to_string())?.map_err(|err| err.to_string())?;

        let response = read_upload_response(&mut std_stream, &cancel)
            .map_err(|err| err.to_string())?;
        return parse_upload_response(&response).map_err(|err| err.to_string());
    }

    let mut connection_count_cfg = req.connections.clamp(1, MAX_PARALLEL_CONNECTIONS);
    let mut optimize_compression: Option<CompressionMode> = None;
    let mut optimize_connections: Option<usize> = None;
    
    let source_path_clone = source_path.clone();
    let cancel_clone = cancel.clone();
    if req.optimize_upload {
        emit_log(handle, run_id, "Optimize upload: sampling files...".to_string());
        let opt = tauri::async_runtime::spawn_blocking(move || {
            ps5upload_core::transfer_utils::optimize_upload_settings(
                &source_path_clone,
                &cancel_clone,
                connection_count_cfg,
            )
        }).await.map_err(|e| e.to_string())?;
        
        optimize_connections = opt.connections;
        optimize_compression = opt.compression;
        if let Some(recommended) = optimize_connections {
            connection_count_cfg = recommended;
        }
        let comp_label = optimize_compression
            .map(compression_label)
            .unwrap_or("Unchanged");
        emit_log(
            handle,
            run_id,
            format!(
                "Optimize upload: compression {}, connections {}",
                comp_label, connection_count_cfg
            ),
        );
    } else if req.auto_tune_connections {
        let source_path_clone = source_path.clone();
        let cancel_clone = cancel.clone();
        if let Some((sample_count, sample_bytes)) = tauri::async_runtime::spawn_blocking(move || sample_workload(&source_path_clone, &cancel_clone)).await.map_err(|e| e.to_string())? {
            let recommended = recommend_connections(connection_count_cfg, sample_count, sample_bytes);
            if recommended != connection_count_cfg {
                emit_log(
                    handle,
                    run_id,
                    format!(
                        "Auto-tune: using {} connection{} for better throughput.",
                        recommended,
                        if recommended == 1 { "" } else { "s" }
                    ),
                );
                connection_count_cfg = recommended;
            }
        }
    }

    let can_stream = req.resume_mode == "none";
    if can_stream {
        emit_log(
            handle,
            run_id,
            format!(
                "Starting streaming upload ({} connections)...",
                connection_count_cfg
            ),
        );

        let tx_handle = handle.clone();
        let shared_total = Arc::new(AtomicU64::new(0));
        let shared_total_scan = shared_total.clone();
        let source_path_clone = source_path.clone();
        let cancel_clone = cancel.clone();
        let rx = tauri::async_runtime::spawn_blocking(move || {
            stream_files_with_progress(source_path_clone, cancel_clone, move |count, total| {
                shared_total_scan.store(total, Ordering::Relaxed);
                emit_scan(&tx_handle, run_id, count, total);
            })
        }).await.map_err(|e| e.to_string())?;

        let start = std::time::Instant::now();
        let last_progress_ms = Arc::new(AtomicU64::new(0));
        let mut compression = match req.compression.to_lowercase().as_str() {
            "auto" => {
                emit_log(handle, run_id, "Auto compression: sampling...".to_string());
                let source_path_clone = source_path.clone();
                let cancel_clone = cancel.clone();
                if let Some(sample) = tauri::async_runtime::spawn_blocking(move || sample_bytes_from_path(&source_path_clone, &cancel_clone)).await.map_err(|e| e.to_string())? {
                    let mode = choose_best_compression(&sample);
                    emit_log(
                        handle,
                        run_id,
                        format!("Auto compression: {}", compression_label(mode)),
                    );
                    mode
                } else {
                    CompressionMode::None
                }
            }
            _ => parse_compression_mode(&req.compression),
        };

        if let Some(override_mode) = optimize_compression {
            compression = override_mode;
        }

        if matches!(compression, CompressionMode::Zstd | CompressionMode::Lzma) && !payload_modern {
            emit_log(
                handle,
                run_id,
                "Payload does not support Zstd/LZMA yet; falling back to LZ4.".to_string(),
            );
            compression = CompressionMode::Lz4;
        }

        let rate_limit = if bandwidth_limit_bps > 0 {
            let per_conn = (bandwidth_limit_bps / connection_count_cfg as u64).max(1);
            Some(per_conn)
        } else {
            None
        };
        
        let ip_clone = ip.clone();
        let dest_path_clone = dest_path.clone();
        let handle_clone = handle.clone();
        let cancel_clone = cancel.clone();
        let transfer_result = tauri::async_runtime::spawn_blocking(move || {
            if connection_count_cfg == 1 {
                let stream = tauri::async_runtime::block_on(async {
                    upload_v2_init(&ip_clone, TRANSFER_PORT, &dest_path_clone, use_temp).await
                })
                .map_err(|err| err.to_string())?;
                let mut std_stream = stream.into_std().map_err(|err| err.to_string())?;
                std_stream
                    .set_nonblocking(true)
                    .map_err(|err| err.to_string())?;
    
                let mut last_sent = 0u64;
                let progress_handle = handle_clone.clone();
                let log_handle = handle_clone.clone();
                send_files_v2_for_list(
                    rx,
                    std_stream.try_clone().map_err(|err| err.to_string())?,
                    SendFilesConfig {
                        cancel: cancel_clone.clone(),
                        progress: move |sent, files_sent, current_file| {
                            if sent == last_sent {
                                return;
                            }
                            let elapsed = start.elapsed().as_secs_f64();
                            let current_total = shared_total.load(Ordering::Relaxed);
                            let display_total = current_total.max(sent);
                            emit_progress(
                                &progress_handle,
                                run_id,
                                sent,
                                display_total,
                                files_sent,
                                elapsed,
                                current_file,
                            );
                            last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            last_sent = sent;
                        },
                        log: move |msg| emit_log(&log_handle, run_id, msg),
                        worker_id: 0,
                        allowed_connections: None,
                        compression,
                        rate_limit_bps: rate_limit,
                    },
                )
                .map_err(|err| err.to_string())?;
    
                let response = read_upload_response(&mut std_stream, &cancel_clone)
                    .map_err(|err| err.to_string())?;
                return parse_upload_response(&response).map_err(|err| err.to_string());
            }

            let shared_rx = Arc::new(std::sync::Mutex::new(rx));
            let total_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let total_files = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let allowed_connections = Arc::new(std::sync::atomic::AtomicUsize::new(connection_count_cfg));

            let mut streams = Vec::new();
            for _ in 0..connection_count_cfg {
                let stream = tauri::async_runtime::block_on(async {
                    upload_v2_init(&ip_clone, TRANSFER_PORT, &dest_path_clone, false).await
                })
                .map_err(|err| err.to_string())?;
                let std_stream = stream.into_std().map_err(|err| err.to_string())?;
                std_stream
                    .set_nonblocking(true)
                    .map_err(|err| err.to_string())?;
                streams.push(std_stream);
            }
    
            let mut handles = Vec::new();
            for (worker_id, std_stream) in streams.into_iter().enumerate() {
                let iterator = SharedReceiverIterator::new(shared_rx.clone());
                let cancel = cancel_clone.clone();
                let total_sent = total_sent.clone();
                let total_files = total_files.clone();
                let shared_total = shared_total.clone();
                let allowed = allowed_connections.clone();
                let last_progress = last_progress_ms.clone();
                let progress_handle = handle_clone.clone();
                let log_handle = handle_clone.clone();
    
                handles.push(std::thread::spawn(move || -> Result<(), String> {
                    let mut last_sent = 0u64;
                    let mut last_files = 0i32;
    
                    send_files_v2_for_list(
                        iterator,
                        std_stream,
                        SendFilesConfig {
                            cancel,
                            progress: move |sent, files_sent, _| {
                                let delta_bytes = sent.saturating_sub(last_sent);
                                let delta_files = if files_sent >= last_files {
                                    files_sent - last_files
                                } else {
                                    0
                                };
                                if delta_bytes == 0 && delta_files == 0 {
                                    return;
                                }
                                last_sent = sent;
                                last_files = files_sent;
    
                                let new_total =
                                    total_sent.fetch_add(delta_bytes, Ordering::Relaxed) + delta_bytes;
                                let new_files =
                                    total_files.fetch_add(delta_files as usize, Ordering::Relaxed)
                                        + delta_files as usize;
                                let elapsed = start.elapsed().as_secs_f64();
                                let current_total_scan = shared_total.load(Ordering::Relaxed);
                                let display_total = current_total_scan.max(new_total);
    
                                emit_progress(
                                    &progress_handle,
                                    run_id,
                                    new_total,
                                    display_total,
                                    new_files as i32,
                                    elapsed,
                                    None,
                                );
                                last_progress.store(
                                    start.elapsed().as_millis() as u64,
                                    Ordering::Relaxed,
                                );
                            },
                            log: move |msg| emit_log(&log_handle, run_id, msg),
                            worker_id,
                            allowed_connections: Some(allowed),
                            compression,
                            rate_limit_bps: rate_limit,
                        },
                    )
                    .map_err(|err| err.to_string())
                }));
            }
    
            let mut first_err: Option<String> = None;
            for h in handles {
                if let Ok(Err(e)) = h.join() {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            if let Some(e) = first_err {
                return Err(e.to_string());
            }
    
            return Ok((
                total_files.load(Ordering::Relaxed) as i32,
                total_sent.load(Ordering::Relaxed),
            ));
        }).await.map_err(|e| e.to_string())?;

        return transfer_result;
    }

    let source_path_clone = source_path.clone();
    let cancel_clone = cancel.clone();
    let handle_clone = handle.clone();
    let (mut files, was_cancelled) = tauri::async_runtime::spawn_blocking(move || {
        collect_files_with_progress(
            &source_path_clone,
            cancel_clone,
            |files_found, total_size| emit_scan(&handle_clone, run_id, files_found, total_size),
        )
    }).await.map_err(|e| e.to_string())?;

    if was_cancelled {
        return Err("Cancelled".to_string());
    }

    if files.is_empty() {
        return Err("No files found to upload".to_string());
    }

    if req.resume_mode != "none" {
        emit_log(handle, run_id, "Resume: scanning remote files...".to_string());
        let dest_exists = check_dir(&ip, TRANSFER_PORT, &dest_path).await.unwrap_or(false);
        
        let remote = if dest_exists {
            list_dir_recursive(&ip, TRANSFER_PORT, &dest_path).await
            .map_err(|err| format!("Resume failed: {}", err))?
        } else {
            std::collections::HashMap::new()
        };

        let mut kept = Vec::with_capacity(files.len());
        let mut skipped_files = 0u64;
        let mut skipped_bytes = 0u64;
        
        let ip_clone = ip.clone();
        let dest_path_clone = dest_path.clone();
        for file in files.into_iter() {
            let Some(remote_entry) = remote.get(&file.rel_path) else {
                kept.push(file);
                continue;
            };
            let mut skip = false;
            match req.resume_mode.as_str() {
                "size" => {
                    skip = remote_entry.size == file.size;
                }
                "size_mtime" => {
                    if remote_entry.size == file.size {
                        skip = match (remote_entry.mtime, file.mtime) {
                            (Some(rm), Some(lm)) => rm == lm,
                            _ => false,
                        };
                    }
                }
                "sha256" => {
                    if remote_entry.size == file.size {
                        let abs_path_clone = file.abs_path.clone();
                        let local_hash = tauri::async_runtime::spawn_blocking(move || ps5upload_core::transfer_utils::sha256_file(&abs_path_clone)).await.map_err(|e| e.to_string())?.map_err(|err| err.to_string())?;
                        
                        let remote_path = format!(
                            "{}/{}",
                            dest_path_clone.trim_end_matches('/'),
                            file.rel_path
                        );
                        let remote_hash = hash_file(&ip_clone, TRANSFER_PORT, &remote_path).await.map_err(|err| err.to_string());

                        if let Ok(remote_hash) = remote_hash {
                            skip = local_hash.eq_ignore_ascii_case(&remote_hash);
                        }
                    }
                }
                _ => {}
            }

            if skip {
                skipped_files += 1;
                skipped_bytes += file.size;
            } else {
                kept.push(file);
            }
        }

        files = kept;
        if skipped_files > 0 {
            emit_log(
                handle,
                run_id,
                format!(
                    "Resume: skipped {} file{} ({})",
                    skipped_files,
                    if skipped_files == 1 { "" } else { "s" },
                    format_bytes(skipped_bytes)
                ),
            );
        }
        if files.is_empty() {
            emit_log(handle, run_id, "Resume: nothing left to upload.".to_string());
            return Ok((0, 0));
        }
    }

    let total_size: u64 = files.iter().map(|f| f.size).sum();
    let mut connection_count = req.connections.clamp(1, MAX_PARALLEL_CONNECTIONS);
    if let Some(recommended) = optimize_connections {
        connection_count = recommended;
    }
    let source_path_clone = source_path.clone();
    let cancel_clone = cancel.clone();
    if let Some((sample_count, sample_bytes)) = tauri::async_runtime::spawn_blocking(move || sample_workload(&source_path_clone, &cancel_clone)).await.map_err(|e| e.to_string())? {
        let recommended = recommend_connections(connection_count, sample_count, sample_bytes);
        connection_count = connection_count.min(recommended);
    }
    if files.len() < connection_count {
        connection_count = files.len().max(1);
    }

    let mut effective_use_temp = use_temp;
    if connection_count > 1 && effective_use_temp {
        effective_use_temp = false;
        emit_log(
            handle,
            run_id,
            "Temp staging disabled for multi-connection uploads.".to_string(),
        );
    }

    emit_log(
        handle,
        run_id,
        format!(
            "Starting transfer: {:.2} GB using {} connection{}",
            total_size as f64 / 1_073_741_824.0,
            connection_count,
            if connection_count == 1 { "" } else { "s" }
        ),
    );

    let start = std::time::Instant::now();
    let last_progress_ms = Arc::new(AtomicU64::new(0));

    let rate_limit = if bandwidth_limit_bps > 0 {
        let per_conn = (bandwidth_limit_bps / connection_count as u64).max(1);
        Some(per_conn)
    } else {
        None
    };

    let files_clone = files.clone();
    let cancel_clone = cancel.clone();
    let mut compression = match req.compression.to_lowercase().as_str() {
        "auto" => {
            if let Some(sample) = tauri::async_runtime::spawn_blocking(move || sample_bytes_from_files(&files_clone, &cancel_clone)).await.map_err(|e| e.to_string())? {
                choose_best_compression(&sample)
            } else {
                CompressionMode::None
            }
        }
        _ => parse_compression_mode(&req.compression),
    };
    if let Some(override_mode) = optimize_compression {
        compression = override_mode;
    }

    if matches!(compression, CompressionMode::Zstd | CompressionMode::Lzma) && !payload_modern {
        emit_log(
            handle,
            run_id,
            "Payload does not support Zstd/LZMA yet; falling back to LZ4.".to_string(),
        );
        compression = CompressionMode::Lz4;
    }
    
    let ip_clone = ip.clone();
    let dest_path_clone = dest_path.clone();
    let handle_clone = handle.clone();
    let cancel_clone = cancel.clone();

    tauri::async_runtime::spawn_blocking(move || {
        if connection_count == 1 {
            let stream = tauri::async_runtime::block_on(async {
                upload_v2_init(&ip_clone, TRANSFER_PORT, &dest_path_clone, effective_use_temp).await
            })
            .map_err(|err| err.to_string())?;
            let mut std_stream = stream.into_std().map_err(|err| err.to_string())?;
            std_stream
                .set_nonblocking(true)
                .map_err(|err| err.to_string())?;
    
            let mut last_sent = 0u64;
            let progress_handle = handle_clone.clone();
            let log_handle = handle_clone.clone();
            send_files_v2_for_list(
                files,
                std_stream.try_clone().map_err(|err| err.to_string())?,
                SendFilesConfig {
                    cancel: cancel_clone.clone(),
                    progress: move |sent, files_sent, current| {
                        if sent == last_sent {
                            return;
                        }
                        let elapsed = start.elapsed().as_secs_f64();
                        emit_progress(
                            &progress_handle,
                            run_id,
                            sent,
                            total_size,
                            files_sent,
                            elapsed,
                            current,
                        );
                        last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                        last_sent = sent;
                    },
                    log: move |msg| emit_log(&log_handle, run_id, msg),
                    worker_id: 0,
                    allowed_connections: None,
                    compression,
                    rate_limit_bps: rate_limit,
                },
            )
            .map_err(|err| err.to_string())?;
    
            let response = read_upload_response(&mut std_stream, &cancel_clone)
                .map_err(|err| err.to_string())?;
            return parse_upload_response(&response).map_err(|err| err.to_string());
        }

        let buckets = partition_files_by_size(files, connection_count);
        let total_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let total_files = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let allowed_connections = Arc::new(std::sync::atomic::AtomicUsize::new(connection_count));
        let mut handles = Vec::new();
    
        let mut workers = Vec::new();
        for bucket in buckets.into_iter().filter(|b| !b.is_empty()) {
            if cancel_clone.load(Ordering::Relaxed) {
                return Err("Upload cancelled".to_string());
            }
            let stream = tauri::async_runtime::block_on(async {
                upload_v2_init(&ip_clone, TRANSFER_PORT, &dest_path_clone, effective_use_temp).await
            })
            .map_err(|err| err.to_string())?;
            let std_stream = stream.into_std().map_err(|err| err.to_string())?;
            std_stream
                .set_nonblocking(true)
                .map_err(|err| err.to_string())?;
            workers.push((bucket, std_stream));
        }
        
        for (worker_id, (bucket, std_stream)) in workers.into_iter().enumerate() {
            let cancel = cancel_clone.clone();
            let total_sent = total_sent.clone();
            let total_files = total_files.clone();
            let allowed = allowed_connections.clone();
            let last_progress = last_progress_ms.clone();
            let progress_handle = handle_clone.clone();
            let log_handle = handle_clone.clone();
    
            handles.push(std::thread::spawn(move || -> Result<(), String> {
                let mut last_sent = 0u64;
                let mut last_files = 0i32;
    
                send_files_v2_for_list(
                    bucket,
                    std_stream,
                    SendFilesConfig {
                        cancel,
                        progress: move |sent, files_sent, current| {
                            let delta_bytes = sent.saturating_sub(last_sent);
                            let delta_files = if files_sent >= last_files {
                                files_sent - last_files
                            } else {
                                0
                            };
                            if delta_bytes == 0 && delta_files == 0 {
                                return;
                            }
                            last_sent = sent;
                            last_files = files_sent;
    
                            let new_total =
                                total_sent.fetch_add(delta_bytes, Ordering::Relaxed) + delta_bytes;
                            let new_files =
                                total_files.fetch_add(delta_files as usize, Ordering::Relaxed)
                                    + delta_files as usize;
                            let elapsed = start.elapsed().as_secs_f64();
    
                        emit_progress(
                                &progress_handle,
                                run_id,
                                new_total,
                                total_size.max(new_total),
                                new_files as i32,
                                elapsed,
                                current,
                            );
                        last_progress.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                    },
                    log: move |msg| emit_log(&log_handle, run_id, msg),
                    worker_id,
                    allowed_connections: Some(allowed),
                    compression,
                        rate_limit_bps: rate_limit,
                    },
                )
                .map_err(|err| err.to_string())
            }));
        }
    
        let mut first_err: Option<String> = None;
        for h in handles {
            if let Ok(Err(e)) = h.join() {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e.to_string());
        }
    
        Ok((
            total_files.load(Ordering::Relaxed) as i32,
            total_sent.load(Ordering::Relaxed),
        ))
    }).await.map_err(|e| e.to_string())?
}
