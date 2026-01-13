/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::{Read, Write};
use tokio::runtime::Runtime;
use serde::Deserialize;

mod protocol;
mod archive;
mod transfer;
mod config;
mod profiles;
mod history;
mod queue;

use protocol::{StorageLocation, DirEntry, list_storage, list_dir, check_dir, upload_v2_init, delete_path, move_path, copy_path, chmod_777, download_file};
use archive::get_size;
use transfer::{collect_files, send_files_v2_for_list, FileEntry};
use config::AppConfig;
use profiles::{Profile, ProfilesData, load_profiles, save_profiles};
use history::{TransferRecord, HistoryData, load_history, add_record, clear_history};
use queue::{QueueItem, QueueData, QueueStatus, load_queue, save_queue};

const PRESETS: [&str; 3] = ["etaHEN/games", "homebrew", "custom"];
const TRANSFER_PORT: u16 = 9113;
const PAYLOAD_PORT: u16 = 9021;
const MAX_PARALLEL_CONNECTIONS: usize = 10; // Increased back to 10 for better saturation

enum AppMessage {
    Log(String),
    PayloadLog(String),
    PayloadSendComplete(Result<u64, String>),
    StorageList(Result<Vec<StorageLocation>, String>),
    ManageList(Result<Vec<DirEntry>, String>),
    ManageOpComplete { op: String, result: Result<(), String> },
    UpdateCheckComplete(Result<ReleaseInfo, String>),
    UpdateDownloadComplete { kind: String, result: Result<String, String> },
    CheckExistsResult(bool),
    SizeCalculated(u64),
    UploadStart,
    Progress { sent: u64, total: u64, files_sent: i32, elapsed_secs: f64, current_file: Option<String> },
    UploadComplete(Result<(i32, u64), String>),
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseInfo {
    tag_name: String,
    html_url: String,
    assets: Vec<ReleaseAsset>,
}

struct Ps5UploadApp {
    // UI State
    ip: String,
    main_tab: usize, // 0 = Transfer, 1 = Manage
    
    // Source
    game_path: String,
    
    // Destination
    selected_storage: Option<String>,
    selected_preset: usize,
    custom_preset_path: String,
    custom_subfolder: String, // Calculated from game_path usually

    storage_locations: Vec<StorageLocation>,

    // Manage
    manage_path: String,
    manage_entries: Vec<DirEntry>,
    manage_selected: Option<usize>,
    manage_status: String,
    manage_target_dir: String,
    manage_new_name: String,
    manage_clipboard: Option<String>,
    manage_busy: bool,

    // Updates
    update_info: Option<ReleaseInfo>,
    update_status: String,
    update_available: bool,
    update_check_running: bool,
    update_download_status: String,
    
    client_logs: String,
    payload_logs: String,
    status: String,
    is_uploading: bool,
    is_connecting: bool,
    is_sending_payload: bool,
    
    // Cancellation
    upload_cancellation_token: Arc<AtomicBool>,
    
    // Override Dialog
    show_override_dialog: bool,
    
    // Progress
    progress_sent: u64,
    progress_total: u64,
    progress_speed_bps: f64,
    progress_eta_secs: Option<f64>,
    progress_files: i32,
    progress_current_file: String,
    
    // Size Calc
    calculating_size: bool,
    calculated_size: Option<u64>,
    
    // Payload
    payload_path: String,

    // UI Toggles
    log_tab: usize, // 0 = Client, 1 = Payload, 2 = History
    theme_dark: bool,

    // Concurrency
    rx: Receiver<AppMessage>,
    tx: Sender<AppMessage>,
    rt: Arc<Runtime>,
    
    // Config
    config: AppConfig,

    // Profiles
    profiles_data: ProfilesData,
    current_profile: Option<String>,
    show_profile_dialog: bool,
    editing_profile: Option<Profile>,
    profile_name_input: String,

    // History
    history_data: HistoryData,
    upload_start_time: Option<std::time::Instant>,
    upload_source_path: String,
    upload_dest_path: String,

    // Queue
    queue_data: QueueData,
    current_queue_item_id: Option<u64>,
}

impl Ps5UploadApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        let (tx, rx) = channel();
        let rt = Runtime::new().expect("Failed to create Tokio runtime");

        let config = AppConfig::load();
        let theme_dark = config.theme != "light";
        let profiles_data = load_profiles();
        let history_data = load_history();
        let queue_data = load_queue();

        // Apply theme based on config
        if theme_dark {
            setup_custom_style(&cc.egui_ctx);
        } else {
            setup_light_style(&cc.egui_ctx);
        }

        let mut app = Self {
            ip: config.address.clone(),
            main_tab: 0,
            game_path: String::new(),
            selected_storage: Some(config.storage.clone()),
            selected_preset: 0,
            custom_preset_path: String::new(),
            custom_subfolder: String::new(),
            storage_locations: Vec::new(),
            manage_path: "/data".to_string(),
            manage_entries: Vec::new(),
            manage_selected: None,
            manage_status: "Not connected".to_string(),
            manage_target_dir: "/data".to_string(),
            manage_new_name: String::new(),
            manage_clipboard: None,
            manage_busy: false,
            update_info: None,
            update_status: "Checking for updates...".to_string(),
            update_available: false,
            update_check_running: false,
            update_download_status: String::new(),
            client_logs: String::new(),
            payload_logs: String::new(),
            status: "Ready".to_string(),
            is_uploading: false,
            is_connecting: false,
            is_sending_payload: false,
            upload_cancellation_token: Arc::new(AtomicBool::new(false)),
            show_override_dialog: false,
            progress_sent: 0,
            progress_total: 0,
            progress_speed_bps: 0.0,
            progress_eta_secs: None,
            progress_files: 0,
            progress_current_file: String::new(),
            calculating_size: false,
            calculated_size: None,
            payload_path: String::new(),
            log_tab: 0,
            theme_dark,
            rx,
            tx,
            rt: Arc::new(rt),
            config,
            profiles_data,
            current_profile: None,
            show_profile_dialog: false,
            editing_profile: None,
            profile_name_input: String::new(),
            history_data,
            upload_start_time: None,
            upload_source_path: String::new(),
            upload_dest_path: String::new(),
            queue_data,
            current_queue_item_id: None,
        };

        app.start_update_check();
        app
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.theme_dark = !self.theme_dark;
        if self.theme_dark {
            setup_custom_style(ctx);
            self.config.theme = "dark".to_string();
        } else {
            setup_light_style(ctx);
            self.config.theme = "light".to_string();
        }
        self.config.save();
    }

    fn apply_profile(&mut self, profile: &Profile) {
        self.ip = profile.address.clone();
        self.selected_storage = Some(profile.storage.clone());
        self.selected_preset = profile.preset_index;
        self.custom_preset_path = profile.custom_preset_path.clone();
        self.config.connections = profile.connections;
        self.config.use_temp = profile.use_temp;
        self.config.address = profile.address.clone();
        self.config.storage = profile.storage.clone();
        self.config.save();
        self.current_profile = Some(profile.name.clone());
    }

    fn save_current_as_profile(&mut self, name: String) {
        let profile = Profile {
            name: name.clone(),
            address: self.ip.clone(),
            storage: self.selected_storage.clone().unwrap_or_else(|| "/data".to_string()),
            preset_index: self.selected_preset,
            custom_preset_path: self.custom_preset_path.clone(),
            connections: self.config.connections,
            use_temp: self.config.use_temp,
        };

        // Update or add profile
        if let Some(existing) = self.profiles_data.profiles.iter_mut().find(|p| p.name == name) {
            *existing = profile;
        } else {
            self.profiles_data.profiles.push(profile);
        }
        save_profiles(&self.profiles_data);
        self.current_profile = Some(name);
    }

    fn delete_profile(&mut self, name: &str) {
        self.profiles_data.profiles.retain(|p| p.name != name);
        if self.profiles_data.default_profile.as_deref() == Some(name) {
            self.profiles_data.default_profile = None;
        }
        if self.current_profile.as_deref() == Some(name) {
            self.current_profile = None;
        }
        save_profiles(&self.profiles_data);
    }

    fn add_to_queue(&mut self) {
        if self.game_path.trim().is_empty() {
            return;
        }

        let subfolder = if self.custom_subfolder.is_empty() {
            Path::new(&self.game_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "App".to_string())
        } else {
            self.custom_subfolder.clone()
        };

        let item = QueueItem {
            id: self.queue_data.next_id,
            source_path: self.game_path.clone(),
            subfolder_name: subfolder,
            preset_index: self.selected_preset,
            custom_preset_path: self.custom_preset_path.clone(),
            status: QueueStatus::Pending,
            size_bytes: self.calculated_size,
        };

        self.queue_data.next_id += 1;
        self.queue_data.items.push(item);
        save_queue(&self.queue_data);
        self.log("Added to queue");
    }

    fn remove_from_queue(&mut self, id: u64) {
        self.queue_data.items.retain(|i| i.id != id);
        save_queue(&self.queue_data);
    }

    fn clear_completed_queue(&mut self) {
        self.queue_data.items.retain(|i| {
            !matches!(i.status, QueueStatus::Completed | QueueStatus::Failed(_))
        });
        save_queue(&self.queue_data);
    }

    fn process_next_queue_item(&mut self) {
        if self.is_uploading {
            return;
        }

        // Find next pending item
        if let Some(item) = self.queue_data.items.iter_mut().find(|i| i.status == QueueStatus::Pending) {
            item.status = QueueStatus::InProgress;
            let item_clone = item.clone();
            save_queue(&self.queue_data);

            // Set up state for upload
            self.game_path = item_clone.source_path.clone();
            self.custom_subfolder = item_clone.subfolder_name.clone();
            self.selected_preset = item_clone.preset_index;
            self.custom_preset_path = item_clone.custom_preset_path.clone();
            self.calculated_size = item_clone.size_bytes;
            self.current_queue_item_id = Some(item_clone.id);

            self.start_upload();
        }
    }

    fn update_queue_item_status(&mut self, id: u64, status: QueueStatus) {
        if let Some(item) = self.queue_data.items.iter_mut().find(|i| i.id == id) {
            item.status = status;
            save_queue(&self.queue_data);
        }
    }

    fn log(&mut self, msg: &str) {
        let time = chrono::Local::now().format("%H:%M:%S");
        self.client_logs.push_str(&format!("[{}] {}\n", time, msg));
    }

    fn payload_log(&mut self, msg: &str) {
        let time = chrono::Local::now().format("%H:%M:%S");
        self.payload_logs.push_str(&format!("[{}] {}\n", time, msg));
    }

    fn join_remote_path(base: &str, name: &str) -> String {
        if base.ends_with('/') {
            format!("{}{}", base, name)
        } else {
            format!("{}/{}", base, name)
        }
    }

    fn manage_refresh(&mut self) {
        if self.ip.trim().is_empty() || self.manage_path.trim().is_empty() {
            return;
        }

        self.manage_busy = true;
        self.manage_status = "Listing...".to_string();
        let ip = self.ip.clone();
        let path = self.manage_path.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();

        thread::spawn(move || {
            let res = rt.block_on(async { list_dir(&ip, TRANSFER_PORT, &path).await });
            let _ = tx.send(AppMessage::ManageList(res.map_err(|e| e.to_string())));
        });
    }

    fn manage_send_op(&mut self, op: &str, task: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.ip.trim().is_empty() {
            return;
        }
        self.manage_busy = true;
        self.manage_status = format!("{}...", op);
        let op_name = op.to_string();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = task();
            let _ = tx.send(AppMessage::ManageOpComplete { op: op_name, result });
        });
    }

    fn start_update_check(&mut self) {
        if self.update_check_running {
            return;
        }
        self.update_check_running = true;
        self.update_status = "Checking for updates...".to_string();
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        thread::spawn(move || {
            let res = rt.block_on(async { fetch_latest_release().await });
            let _ = tx.send(AppMessage::UpdateCheckComplete(res.map_err(|e| e.to_string())));
        });
    }

    fn start_download_asset(&mut self, kind: &str, asset_name: &str, default_filename: &str) {
        let Some(release) = self.update_info.clone() else {
            self.update_download_status = "No release info yet.".to_string();
            return;
        };

        let asset = release.assets.iter().find(|a| a.name == asset_name);
        let Some(asset) = asset else {
            self.update_download_status = format!("Asset not found: {}", asset_name);
            return;
        };

        let save_path = rfd::FileDialog::new().set_file_name(default_filename).save_file();
        let Some(save_path) = save_path else {
            return;
        };

        self.update_download_status = format!("Downloading {}...", kind);
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        let url = asset.browser_download_url.clone();
        let save_path = save_path.display().to_string();
        let kind_name = kind.to_string();

        thread::spawn(move || {
            let result = rt.block_on(async { download_asset(&url, &save_path).await })
                .map(|_| save_path)
                .map_err(|e| e.to_string());
            let _ = tx.send(AppMessage::UpdateDownloadComplete { kind: kind_name, result });
        });
    }
    
    fn build_dest_path_with_base(&self, base: &str) -> String {
        let base = if base.trim().is_empty() { "/data" } else { base };
        
        let preset_path = if self.selected_preset == 2 { // custom
             &self.custom_preset_path
        } else {
             PRESETS[self.selected_preset]
        };

        let folder = if self.custom_subfolder.is_empty() {
             "App"
        } else {
             &self.custom_subfolder
        };
        
        let base_clean = base.trim_end_matches('/');
        let preset_clean = preset_path.trim_matches('/');
        
        if preset_clean.is_empty() {
            format!("{}/{}", base_clean, folder)
        } else {
            format!("{}/{}/{}", base_clean, preset_clean, folder)
        }
    }

    fn get_dest_path(&self) -> String {
        let base = self.selected_storage.as_deref().unwrap_or("/data");
        self.build_dest_path_with_base(base)
    }

    fn update_game_path(&mut self, path: String) {
        self.game_path = path;
        let path_obj = Path::new(&self.game_path);
        if let Some(name) = path_obj.file_name() {
            self.custom_subfolder = name.to_string_lossy().to_string();
        }
        
        self.calculating_size = true;
        self.calculated_size = None;
        let path_clone = self.game_path.clone();
        let tx = self.tx.clone();
        
        thread::spawn(move || {
            let size = get_size(&path_clone);
            let _ = tx.send(AppMessage::SizeCalculated(size));
        });
    }
    
    fn connect(&mut self) {
        // Save config
        self.config.address = self.ip.clone();
        self.config.save();

        self.is_connecting = true;
        self.status = "Connecting...".to_string();
        self.log(&format!("Connecting to {}...", self.ip));
        
        let ip = self.ip.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        
        thread::spawn(move || {
            rt.block_on(async {
                match list_storage(&ip, TRANSFER_PORT).await {
                    Ok(locs) => {
                        let _ = tx.send(AppMessage::StorageList(Ok(locs)));
                    }
                    Err(e) => {
                        let _ = tx.send(AppMessage::StorageList(Err(e.to_string())));
                    }
                }
            });
        });
    }
    
    fn check_exists_and_upload(&mut self) {
         if self.game_path.trim().is_empty() {
            self.log("Please select an app folder first.");
            return;
        }

        let dest = self.get_dest_path();
        let ip = self.ip.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();

        self.status = "Checking destination...".to_string();

        thread::spawn(move || {
            rt.block_on(async {
                match check_dir(&ip, TRANSFER_PORT, &dest).await {
                    Ok(exists) => {
                         let _ = tx.send(AppMessage::CheckExistsResult(exists));
                    }
                    Err(_) => {
                        let _ = tx.send(AppMessage::CheckExistsResult(false));
                    }
                }
            });
        });
    }

    fn start_upload(&mut self) {
        // Reset state
        self.is_uploading = true;
        self.show_override_dialog = false;
        self.status = "Uploading...".to_string();
        self.progress_sent = 0;
        self.progress_total = self.calculated_size.unwrap_or(0);
        self.progress_files = 0;
        self.progress_speed_bps = 0.0;
        self.progress_eta_secs = None;

        // Record for history
        self.upload_start_time = Some(std::time::Instant::now());
        self.upload_source_path = self.game_path.clone();
        self.upload_dest_path = self.get_dest_path();

        self.upload_cancellation_token.store(false, Ordering::Relaxed);
        let cancel_token = self.upload_cancellation_token.clone();
        
        let ip = self.ip.clone();
        let game_path = self.game_path.clone();
        let dest_path = self.get_dest_path();
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        let connections = self.config.connections;
        let use_temp = self.config.use_temp;
        
        thread::spawn(move || {
            let tx_log = tx.clone();
            let res = rt.block_on(async {
                if cancel_token.load(Ordering::Relaxed) {
                    return Err(anyhow::anyhow!("Cancelled"));
                }

                let files = collect_files(&game_path);
                if files.is_empty() {
                    return Err(anyhow::anyhow!("No files found to upload"));
                }

                let total_size: u64 = files.iter().map(|f| f.size).sum();
                let mut connection_count = connections.clamp(1, MAX_PARALLEL_CONNECTIONS);
                if files.len() < connection_count {
                    connection_count = files.len().max(1);
                }

                let _ = tx.send(AppMessage::Log(format!(
                    "Starting transfer: {:.2} GB using {} connection{}",
                    total_size as f64 / 1_073_741_824.0,
                    connection_count,
                    if connection_count == 1 { "" } else { "s" }
                )));
                let _ = tx.send(AppMessage::UploadStart);

                let start = std::time::Instant::now();
                let last_progress_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
                if connection_count == 1 {
                    let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, use_temp).await?;
                    let mut std_stream = stream.into_std()?;
                    std_stream.set_nonblocking(false)?;
                    let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));

                    let mut last_sent = 0u64;
                    send_files_v2_for_list(
                        files,
                        std_stream.try_clone()?,
                        cancel_token.clone(),
                        |sent, files_sent, current_file| {
                            if sent == last_sent { return; }
                            let elapsed = start.elapsed().as_secs_f64();
                            let _ = tx.send(AppMessage::Progress {
                                sent,
                                total: total_size,
                                files_sent,
                                elapsed_secs: elapsed,
                                current_file,
                            });
                            last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            last_sent = sent;
                        },
                        move |msg| {
                            let _ = tx_log.send(AppMessage::Log(msg));
                        },
                        0,
                        None,
                    )?;

                    use std::io::Read;
                    let mut buffer = [0u8; 1024];
                    let n = std_stream.read(&mut buffer)?;
                    let response = String::from_utf8_lossy(&buffer[..n]).trim().to_string();
                    return parse_upload_response(&response);
                }

                let buckets = partition_files_by_size(files, connection_count);
                let total_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let total_files = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let allowed_connections = Arc::new(std::sync::atomic::AtomicUsize::new(connection_count));
                let mut handles = Vec::new();

                let mut workers = Vec::new();
                for bucket in buckets.into_iter().filter(|b| !b.is_empty()) {
                    let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, use_temp).await?;
                    let std_stream = stream.into_std()?;
                    std_stream.set_nonblocking(false)?;
                    workers.push((bucket, std_stream));
                }
                let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));

                let max_connections = connection_count;
                let allowed_monitor = allowed_connections.clone();
                let last_progress_monitor = last_progress_ms.clone();
                let cancel_monitor = cancel_token.clone();
                let start_monitor = start;
                thread::spawn(move || {
                    let mut stable_good = 0u8;
                    loop {
                        if cancel_monitor.load(Ordering::Relaxed) {
                            break;
                        }
                        let elapsed_ms = start_monitor.elapsed().as_millis() as u64;
                        let last_ms = last_progress_monitor.load(Ordering::Relaxed);
                        if last_ms == 0 {
                            thread::sleep(std::time::Duration::from_millis(500));
                            continue;
                        }
                        let since = elapsed_ms.saturating_sub(last_ms);
                        if since > 2000 {
                            let current = allowed_monitor.load(Ordering::Relaxed);
                            if current > 1 {
                                allowed_monitor.store(current - 1, Ordering::Relaxed);
                            }
                            stable_good = 0;
                        } else if since < 500 {
                            stable_good = stable_good.saturating_add(1);
                            if stable_good >= 6 {
                                let current = allowed_monitor.load(Ordering::Relaxed);
                                if current < max_connections {
                                    allowed_monitor.store(current + 1, Ordering::Relaxed);
                                }
                                stable_good = 0;
                            }
                        } else {
                            stable_good = 0;
                        }
                        thread::sleep(std::time::Duration::from_millis(500));
                    }
                });

                for (worker_id, (bucket, mut std_stream)) in workers.into_iter().enumerate() {
                    let cancel = cancel_token.clone();
                    let tx = tx.clone();
                    let tx_log = tx_log.clone();
                    let total_sent = total_sent.clone();
                    let total_files = total_files.clone();
                    let start = start;
                    let allowed_connections = allowed_connections.clone();
                    let last_progress_ms = last_progress_ms.clone();

                    handles.push(thread::spawn(move || -> anyhow::Result<()> {
                        let mut last_sent = 0u64;
                        let mut last_files = 0i32;
                        send_files_v2_for_list(
                            bucket,
                            std_stream.try_clone()?,
                            cancel.clone(),
                            |sent, files_sent, current_file| {
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

                                let new_total = total_sent.fetch_add(delta_bytes, Ordering::Relaxed) + delta_bytes;
                                let new_files = total_files.fetch_add(delta_files as usize, Ordering::Relaxed) + delta_files as usize;
                                let elapsed = start.elapsed().as_secs_f64();
                                let _ = tx.send(AppMessage::Progress {
                                    sent: new_total,
                                    total: total_size,
                                    files_sent: new_files as i32,
                                    elapsed_secs: elapsed,
                                    current_file,
                                });
                                last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            },
                            move |msg| {
                                let _ = tx_log.send(AppMessage::Log(msg));
                            },
                            worker_id,
                            Some(allowed_connections),
                        )?;

                        use std::io::Read;
                        let mut buffer = [0u8; 1024];
                        let n = std_stream.read(&mut buffer)?;
                        let response = String::from_utf8_lossy(&buffer[..n]).trim().to_string();
                        parse_upload_response(&response).map(|_| ())
                    }));
                }

                let mut first_err: Option<anyhow::Error> = None;
                for handle in handles {
                    match handle.join() {
                        Ok(result) => {
                            if let Err(err) = result {
                                if first_err.is_none() {
                                    first_err = Some(err);
                                    cancel_token.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(_) => {
                            if first_err.is_none() {
                                first_err = Some(anyhow::anyhow!("Upload worker panicked"));
                                cancel_token.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }

                if let Some(err) = first_err {
                    Err(err)
                } else {
                    let files_sent = total_files.load(Ordering::Relaxed) as i32;
                    let bytes_sent = total_sent.load(Ordering::Relaxed);
                    Ok((files_sent, bytes_sent))
                }
            });
            
            let _ = tx.send(AppMessage::UploadComplete(res.map_err(|e| e.to_string())));
        });
    }

    fn send_payload(&mut self) {
        if self.ip.trim().is_empty() {
            self.payload_log("Enter a PS5 address first.");
            return;
        }
        if self.payload_path.trim().is_empty() {
            self.payload_log("Select a payload (.elf) file first.");
            return;
        }

        self.is_sending_payload = true;
        self.status = "Sending payload...".to_string();
        self.payload_log(&format!("Sending payload to {}:{}...", self.ip, PAYLOAD_PORT));
        if !self.payload_path.trim().is_empty() {
            self.payload_log(&format!("Payload path: {}", self.payload_path));
        }

        let ip = self.ip.clone();
        let path = self.payload_path.clone();
        let tx = self.tx.clone();

        thread::spawn(move || {
            use std::fs::File;
            use std::net::TcpStream;
            use std::time::Duration;

            let result = (|| -> Result<u64, String> {
                let mut file = File::open(&path)
                    .map_err(|e| format!("Failed to open payload: {}", e))?;
                let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
                if file_len > 0 {
                    let size_msg = crate::format_bytes(file_len);
                    let _ = tx.send(AppMessage::PayloadLog(format!("Payload size: {}", size_msg)));
                }
                let mut stream = TcpStream::connect((ip.as_str(), PAYLOAD_PORT))
                    .map_err(|e| format!("Failed to connect: {}", e))?;
                let _ = stream.set_nodelay(true);
                let mut buffer = vec![0u8; 256 * 1024];
                let mut sent = 0u64;
                loop {
                    let n = file.read(&mut buffer)
                        .map_err(|e| format!("Send failed: {}", e))?;
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buffer[..n])
                        .map_err(|e| format!("Send failed: {}", e))?;
                    sent += n as u64;
                }
                // Send FIN to indicate we are done writing
                let _ = stream.shutdown(std::net::Shutdown::Write);
                
                // Critical for Windows: Read until server closes connection (EOF) or timeout.
                // If we close the socket while the server sends data (or before it processes FIN), 
                // Windows might send RST, causing the loader to fail.
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
            })();

            let _ = tx.send(AppMessage::PayloadSendComplete(result));
        });
    }
}

impl eframe::App for Ps5UploadApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let connected = self.status == "Connected" || self.is_uploading;

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMessage::Log(s) => self.log(&s),
                AppMessage::PayloadLog(s) => self.payload_log(&s),
                AppMessage::StorageList(res) => {
                    self.is_connecting = false;
                    match res {
                        Ok(locs) => {
                            self.storage_locations = locs.into_iter().filter(|l| l.free_gb > 0.0).collect();
                            if let Some(first) = self.storage_locations.first() {
                                self.selected_storage = Some(first.path.clone());
                                if self.manage_path.trim().is_empty() || self.manage_path == "/data" {
                                    self.manage_path = first.path.clone();
                                    self.manage_target_dir = first.path.clone();
                                }
                            }
                            self.status = "Connected".to_string();
                            self.log("Connected to PS5");
                            self.payload_log("Connected and Storage scanned.");
                            if self.main_tab == 1 {
                                self.manage_refresh();
                            }
                        }
                        Err(e) => {
                            self.log(&format!("Error: {}", e));
                            self.status = "Connection Failed".to_string();
                        }
                    }
                }
                AppMessage::ManageList(res) => {
                    self.manage_busy = false;
                    match res {
                        Ok(mut entries) => {
                            entries.sort_by(|a, b| {
                                let a_is_dir = a.entry_type == "dir";
                                let b_is_dir = b.entry_type == "dir";
                                match b_is_dir.cmp(&a_is_dir) {
                                    std::cmp::Ordering::Equal => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                                    other => other,
                                }
                            });
                            let count = entries.len();
                            self.manage_entries = entries;
                            self.manage_selected = None;
                            self.manage_status = format!("{} item{}", count, if count == 1 { "" } else { "s" });
                        }
                        Err(e) => {
                            self.manage_status = format!("List failed: {}", e);
                        }
                    }
                }
                AppMessage::ManageOpComplete { op, result } => {
                    self.manage_busy = false;
                    match result {
                        Ok(()) => {
                            self.manage_status = format!("{} OK", op);
                            self.manage_refresh();
                        }
                        Err(e) => {
                            self.manage_status = format!("{} failed: {}", op, e);
                        }
                    }
                }
                AppMessage::UpdateCheckComplete(res) => {
                    self.update_check_running = false;
                    match res {
                        Ok(release) => {
                            let current = env!("CARGO_PKG_VERSION");
                            let latest = release.tag_name.clone();
                            match is_newer_version(&latest, current) {
                                Some(true) => {
                                    self.update_available = true;
                                    self.update_status = format!("Update available: {}", release.tag_name);
                                }
                                Some(false) => {
                                    self.update_available = false;
                                    self.update_status = format!("Up to date ({})", release.tag_name);
                                }
                                None => {
                                    let current_norm = normalize_version(current);
                                    let latest_norm = normalize_version(&latest);
                                    self.update_available = latest_norm != current_norm;
                                    self.update_status = if self.update_available {
                                        format!("Update available: {}", release.tag_name)
                                    } else {
                                        format!("Up to date ({})", release.tag_name)
                                    };
                                }
                            }
                            self.update_info = Some(release);
                        }
                        Err(e) => {
                            self.update_status = format!("Update check failed: {}", e);
                            self.update_info = None;
                            self.update_available = false;
                        }
                    }
                }
                AppMessage::UpdateDownloadComplete { kind, result } => {
                    match result {
                        Ok(path) => {
                            self.update_download_status = format!("Downloaded {} to {}", kind, path);
                        }
                        Err(e) => {
                            self.update_download_status = format!("Download {} failed: {}", kind, e);
                        }
                    }
                }
                AppMessage::PayloadSendComplete(res) => {
                    self.is_sending_payload = false;
                    match res {
                        Ok(bytes) => {
                            self.payload_log(&format!("Payload sent ({}).", format_bytes(bytes)));
                            self.status = "Payload sent".to_string();
                        }
                        Err(e) => {
                            self.payload_log(&format!("Payload failed: {}", e));
                            self.status = "Payload failed".to_string();
                        }
                    }
                }
                AppMessage::CheckExistsResult(exists) => {
                    if exists {
                        self.show_override_dialog = true;
                        self.status = "Confirm Overwrite".to_string();
                    } else {
                        self.start_upload();
                    }
                }
                AppMessage::SizeCalculated(size) => {
                    self.calculating_size = false;
                    self.calculated_size = Some(size);
                }
                AppMessage::UploadStart => { self.status = "Uploading...".to_string(); }
                AppMessage::Progress { sent, total, files_sent, elapsed_secs, current_file } => {
                    self.progress_sent = sent;
                    self.progress_total = total;
                    self.progress_files = files_sent;
                    if elapsed_secs > 0.0 { self.progress_speed_bps = sent as f64 / elapsed_secs; }
                    if total > sent && self.progress_speed_bps > 0.0 {
                        let remaining = (total - sent) as f64;
                        self.progress_eta_secs = Some(remaining / self.progress_speed_bps);
                    }
                    if let Some(name) = current_file {
                        self.progress_current_file = name;
                    }
                }
                AppMessage::UploadComplete(res) => {
                    self.is_uploading = false;
                    let duration = self.upload_start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);

                    match &res {
                        Ok((files, bytes)) => {
                            self.progress_files = *files;
                            self.log(&format!("✓ Completed: {} files, {}", files, format_bytes(*bytes)));
                            self.payload_log("SUCCESS");
                            self.status = "Upload Complete!".to_string();
                            if self.progress_total > 0 { self.progress_sent = self.progress_total; }

                            // Record to history
                            let record = TransferRecord {
                                timestamp: chrono::Utc::now().timestamp(),
                                source_path: self.upload_source_path.clone(),
                                dest_path: self.upload_dest_path.clone(),
                                file_count: *files,
                                total_bytes: *bytes,
                                duration_secs: duration,
                                speed_bps: if duration > 0.0 { *bytes as f64 / duration } else { 0.0 },
                                success: true,
                                error: None,
                            };
                            add_record(&mut self.history_data, record);

                            // Update queue item status
                            if let Some(id) = self.current_queue_item_id.take() {
                                self.update_queue_item_status(id, QueueStatus::Completed);
                            }

                            // Process next queue item or reconnect
                            if self.queue_data.items.iter().any(|i| i.status == QueueStatus::Pending) {
                                self.process_next_queue_item();
                            } else if self.config.auto_connect {
                                self.connect();
                            }
                        }
                        Err(e) => {
                            self.log(&format!("✗ Failed: {}", e));
                            self.payload_log(&format!("ERROR: {}", e));
                            self.status = "Upload Failed".to_string();

                            // Record failed transfer to history
                            let record = TransferRecord {
                                timestamp: chrono::Utc::now().timestamp(),
                                source_path: self.upload_source_path.clone(),
                                dest_path: self.upload_dest_path.clone(),
                                file_count: self.progress_files,
                                total_bytes: self.progress_sent,
                                duration_secs: duration,
                                speed_bps: if duration > 0.0 { self.progress_sent as f64 / duration } else { 0.0 },
                                success: false,
                                error: Some(e.clone()),
                            };
                            add_record(&mut self.history_data, record);

                            // Update queue item status
                            if let Some(id) = self.current_queue_item_id.take() {
                                self.update_queue_item_status(id, QueueStatus::Failed(e.clone()));
                            }

                            if self.config.auto_connect && self.upload_cancellation_token.load(Ordering::Relaxed) {
                                self.connect();
                            }
                        }
                    }
                    self.upload_start_time = None;
                }
            }
        }

        if self.show_override_dialog {
            egui::Window::new("Overwrite Confirmation")
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Folder already exists:\n{}

Overwrite it?", self.get_dest_path()));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Overwrite").clicked() { self.start_upload(); }
                        if ui.button("Cancel").clicked() { self.show_override_dialog = false; self.status = "Connected".to_string(); }
                    });
                });
        }

        // Profile Management Dialog
        if self.show_profile_dialog {
            egui::Window::new("Manage Profiles")
                .collapsible(false)
                .resizable(true)
                .default_size([350.0, 300.0])
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.heading("Saved Profiles");
                    ui.add_space(5.0);

                    let mut profile_to_delete: Option<String> = None;
                    let mut profile_to_apply: Option<Profile> = None;

                    egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                        if self.profiles_data.profiles.is_empty() {
                            ui.label("No profiles saved yet.");
                        } else {
                            for profile in &self.profiles_data.profiles {
                                ui.horizontal(|ui| {
                                    ui.label(&profile.name);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("Delete").clicked() {
                                            profile_to_delete = Some(profile.name.clone());
                                        }
                                        if ui.small_button("Load").clicked() {
                                            profile_to_apply = Some(profile.clone());
                                        }
                                    });
                                });
                                ui.separator();
                            }
                        }
                    });

                    if let Some(name) = profile_to_delete {
                        self.delete_profile(&name);
                    }
                    if let Some(profile) = profile_to_apply {
                        self.apply_profile(&profile);
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(5.0);

                    ui.heading("Save Current Settings");
                    ui.horizontal(|ui| {
                        ui.label("Profile Name:");
                        ui.text_edit_singleline(&mut self.profile_name_input);
                    });
                    ui.add_space(5.0);

                    ui.horizontal(|ui| {
                        let can_save = !self.profile_name_input.trim().is_empty();
                        if ui.add_enabled(can_save, egui::Button::new("Save Profile")).clicked() {
                            let name = self.profile_name_input.trim().to_string();
                            self.save_current_as_profile(name);
                            self.profile_name_input.clear();
                        }
                        if ui.button("Close").clicked() {
                            self.show_profile_dialog = false;
                        }
                    });
                });
        }

        // 1. TOP HEADER
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.heading("PS5 Upload");
                    ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("☕ Buy me a coffee").min_size([120.0, 32.0].into())).clicked() {
                        let _ = webbrowser::open("https://ko-fi.com/B0B81S0WUA");
                    }
                    ui.separator();
                    let theme_icon = if self.theme_dark { "🌙" } else { "☀" };
                    if ui.button(theme_icon).on_hover_text("Toggle Theme").clicked() {
                        self.toggle_theme(ctx);
                    }
                });
            });
            ui.add_space(5.0);
        });

        // 2. BOTTOM STATUS BAR
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.status).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                     ui.hyperlink_to("Created by PhantomPtr", "https://x.com/phantomptr");
                     ui.label("|");
                     ui.hyperlink_to("Source Code", "https://github.com/phantomptr/ps5upload");
                });
            });
        });

        // 3. LEFT PANEL: CONNECTION & STORAGE
        egui::SidePanel::left("left_panel").resizable(true).default_width(300.0).min_width(250.0).show(ctx, |ui| {
            ui.add_space(10.0);
            ui.heading("Connect");
            ui.add_space(5.0);

            // Profile selector
            ui.horizontal(|ui| {
                ui.label("Profile:");
                let profile_names: Vec<String> = self.profiles_data.profiles.iter().map(|p| p.name.clone()).collect();
                egui::ComboBox::from_id_source("profile_combo")
                    .selected_text(self.current_profile.as_deref().unwrap_or("(none)"))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(self.current_profile.is_none(), "(none)").clicked() {
                            self.current_profile = None;
                        }
                        for name in &profile_names {
                            if ui.selectable_label(self.current_profile.as_ref() == Some(name), name).clicked() {
                                if let Some(profile) = self.profiles_data.profiles.iter().find(|p| &p.name == name) {
                                    let profile = profile.clone();
                                    self.apply_profile(&profile);
                                }
                            }
                        }
                    });
                if ui.button("+").on_hover_text("Manage Profiles").clicked() {
                    self.show_profile_dialog = true;
                    self.editing_profile = None;
                    self.profile_name_input.clear();
                }
            });
            ui.add_space(5.0);

            ui.label("PS5 Address");
            ui.text_edit_singleline(&mut self.ip);
            ui.label(format!("Transfer port: {}", TRANSFER_PORT));

            ui.add_space(8.0);
            ui.label("Send Payload (optional)");
            ui.label(format!("Payload port: {}", PAYLOAD_PORT));
            ui.horizontal(|ui| {
                if ui.button("📂 Select").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Payload", &["elf"]).pick_file() {
                        self.payload_path = path.display().to_string();
                    }
                }
                ui.text_edit_singleline(&mut self.payload_path);
            });
            ui.add_space(5.0);
            let payload_enabled = !self.is_sending_payload && !self.ip.trim().is_empty() && !self.payload_path.trim().is_empty();
            if ui.add_enabled(payload_enabled, egui::Button::new("📤 Send Payload").min_size([ui.available_width(), 30.0].into())).clicked() {
                self.send_payload();
            }
            ui.add_space(5.0);
            let auto_connect = ui.checkbox(&mut self.config.auto_connect, "Auto reconnect after upload");
            if auto_connect.changed() { self.config.save(); }

            ui.add_space(10.0);
            if connected {
                if ui.add(egui::Button::new("Disconnect").min_size([ui.available_width(), 30.0].into())).clicked() {
                    self.status = "Disconnected".to_string();
                    self.storage_locations.clear();
                    self.selected_storage = None;
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.is_connecting, egui::Button::new("Connect to PS5").min_size([150.0, 30.0].into())).clicked() {
                        self.connect();
                    }
                    if self.is_connecting {
                        if ui.add(egui::Button::new("Stop").min_size([60.0, 30.0].into())).clicked() {
                            self.is_connecting = false;
                            self.status = "Cancelled".to_string();
                        }
                    }
                });
            }
            
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.heading("Storage");
                if connected {
                    if ui.button("⟳ Refresh").clicked() { self.connect(); }
                }
            });
            ui.add_space(5.0);
            
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.storage_locations.is_empty() {
                    ui.label("Not connected");
                } else {
                     egui::Grid::new("storage_grid").striped(true).spacing([10.0, 5.0]).show(ui, |ui| {
                         for loc in &self.storage_locations {
                            if ui.radio_value(&mut self.selected_storage, Some(loc.path.clone()), &loc.path).clicked() {
                                self.config.storage = loc.path.clone();
                                self.config.save();
                            }
                            ui.label(format!("{:.1} GB Free", loc.free_gb));
                            ui.end_row();
                         }
                     });
                }
            });

            ui.add_space(15.0);
            ui.heading("Updates");
            ui.add_space(5.0);
            ui.label(format!("Current: v{}", env!("CARGO_PKG_VERSION")));
            if self.update_available {
                ui.label(egui::RichText::new("New version available").color(egui::Color32::from_rgb(255, 140, 0)));
            }
            ui.label(self.update_status.clone());
            if ui.button("Check Updates").clicked() {
                self.start_update_check();
            }

            if let Some(info) = &self.update_info {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button("Open Release Page").clicked() {
                        let _ = webbrowser::open(&info.html_url);
                    }
                });

                ui.add_space(5.0);
                if ui.button("Download Payload (.elf)").clicked() {
                    self.start_download_asset("payload", "ps5upload.elf", "ps5upload.elf");
                }

                match current_asset_name() {
                    Ok(asset_name) => {
                        if ui.button("Download Client (this OS)").clicked() {
                            self.start_download_asset("client", &asset_name, &asset_name);
                        }
                    }
                    Err(e) => {
                        ui.label(format!("Client download unavailable: {}", e));
                    }
                }
            }
            if !self.update_download_status.is_empty() {
                ui.label(self.update_download_status.clone());
            }
        });

        // 4. RIGHT PANEL: LOGS
        egui::SidePanel::right("right_panel").resizable(true).default_width(450.0).min_width(350.0).show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading("Logs");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗑 Clear").clicked() {
                        self.client_logs.clear();
                        self.payload_logs.clear();
                    }
                });
            });
            
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.log_tab, 0, "Client");
                ui.selectable_value(&mut self.log_tab, 1, "Payload");
                ui.selectable_value(&mut self.log_tab, 2, "History");
            });
            ui.separator();

            if self.log_tab == 2 {
                // History tab
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.history_data.records.is_empty() {
                        ui.label("No transfer history yet.");
                    } else {
                        for record in self.history_data.records.iter().rev() {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    let icon = if record.success { "✓" } else { "✗" };
                                    let color = if record.success {
                                        egui::Color32::from_rgb(100, 200, 100)
                                    } else {
                                        egui::Color32::from_rgb(200, 100, 100)
                                    };
                                    ui.label(egui::RichText::new(icon).color(color).strong());

                                    if let Some(dt) = chrono::DateTime::from_timestamp(record.timestamp, 0) {
                                        ui.label(dt.format("%m/%d %H:%M").to_string());
                                    }
                                });

                                // Source folder name only
                                let source_name = std::path::Path::new(&record.source_path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| record.source_path.clone());
                                ui.label(format!("Source: {}", source_name));

                                ui.horizontal(|ui| {
                                    ui.label(format!("{} files", record.file_count));
                                    ui.separator();
                                    ui.label(format_bytes(record.total_bytes));
                                    ui.separator();
                                    ui.label(format!("{}/s", format_bytes(record.speed_bps as u64)));
                                    ui.separator();
                                    ui.label(format_duration(record.duration_secs));
                                });

                                if let Some(err) = &record.error {
                                    ui.label(egui::RichText::new(format!("Error: {}", err)).color(egui::Color32::from_rgb(200, 100, 100)).small());
                                }
                            });
                            ui.add_space(2.0);
                        }
                    }
                });

                ui.add_space(5.0);
                if ui.button("Clear History").clicked() {
                    clear_history(&mut self.history_data);
                }
            } else {
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    let text = if self.log_tab == 0 { &self.client_logs } else { &self.payload_logs };
                    ui.add(egui::TextEdit::multiline(&mut text.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .lock_focus(true));
                });
            }
        });

        // 5. CENTRAL PANEL: MAIN
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let transfer_tab = ui.selectable_value(&mut self.main_tab, 0, "Transfer");
                let manage_tab = ui.selectable_value(&mut self.main_tab, 1, "Manage");
                if manage_tab.clicked() && connected {
                    self.manage_refresh();
                }
                if transfer_tab.clicked() {
                    self.manage_selected = None;
                }
            });
            ui.separator();

            if self.main_tab == 1 {
                ui.heading("Manage Files");
                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Current Path");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.manage_path);
                        let can_manage = connected && !self.manage_busy;
                        if ui.add_enabled(can_manage, egui::Button::new("Go")).clicked() {
                            self.manage_target_dir = self.manage_path.clone();
                            self.manage_refresh();
                        }
                        if ui.add_enabled(can_manage, egui::Button::new("Up")).clicked() {
                            if let Some(parent) = std::path::Path::new(&self.manage_path).parent() {
                                self.manage_path = parent.display().to_string();
                                self.manage_target_dir = self.manage_path.clone();
                                self.manage_refresh();
                            }
                        }
                        if ui.add_enabled(can_manage, egui::Button::new("Use Storage")).clicked() {
                            if let Some(path) = &self.selected_storage {
                                self.manage_path = path.clone();
                                self.manage_target_dir = path.clone();
                                self.manage_refresh();
                            }
                        }
                        if ui.add_enabled(can_manage, egui::Button::new("Refresh")).clicked() {
                            self.manage_refresh();
                        }
                    });
                    ui.label(self.manage_status.clone());
                });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Directory Contents");
                    if !connected {
                        ui.label("Not connected");
                        return;
                    }
                    let mut open_dir: Option<String> = None;
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        egui::Grid::new("manage_list").striped(true).min_col_width(140.0).show(ui, |ui| {
                            ui.strong("Name");
                            ui.strong("Type");
                            ui.strong("Size");
                            ui.strong("Modified");
                            ui.end_row();

                            for (idx, entry) in self.manage_entries.iter().enumerate() {
                                let is_selected = self.manage_selected == Some(idx);
                                let icon = if entry.entry_type == "dir" { "📁" } else { "📄" };
                                let response = ui.selectable_label(is_selected, format!("{} {}", icon, entry.name));
                                if response.clicked() {
                                    self.manage_selected = Some(idx);
                                    self.manage_new_name = entry.name.clone();
                                }
                                if response.double_clicked() && entry.entry_type == "dir" {
                                    open_dir = Some(entry.name.clone());
                                }
                                ui.label(entry.entry_type.clone());
                                if entry.entry_type == "dir" {
                                    ui.label("--");
                                } else {
                                    ui.label(format_bytes(entry.size));
                                }
                                ui.label(format_modified_time(entry.mtime));
                                ui.end_row();
                            }
                        });
                    });
                    if let Some(dir_name) = open_dir {
                        self.manage_path = Self::join_remote_path(&self.manage_path, &dir_name);
                        self.manage_target_dir = self.manage_path.clone();
                        self.manage_refresh();
                    }
                });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Actions");

                    let selected_info = self.manage_selected
                        .and_then(|idx| self.manage_entries.get(idx))
                        .map(|entry| (entry.name.clone(), entry.entry_type.clone()));

                    if let Some((selected_name, selected_type)) = selected_info {
                        let selected_path = Self::join_remote_path(&self.manage_path, &selected_name);
                        ui.label(format!("Selected: {}", selected_path));

                        let can_ops = connected && !self.manage_busy;

                        ui.horizontal(|ui| {
                            ui.label("Rename to");
                            ui.text_edit_singleline(&mut self.manage_new_name);
                            let can_rename = can_ops
                                && !self.manage_new_name.trim().is_empty()
                                && self.manage_new_name.trim() != selected_name;
                            if ui.add_enabled(can_rename, egui::Button::new("Rename")).clicked() {
                                let src = selected_path.clone();
                                let new_name = self.manage_new_name.trim().to_string();
                                let dst = Self::join_remote_path(&self.manage_path, &new_name);
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("Rename", move || {
                                    rt.block_on(async {
                                        move_path(&ip, TRANSFER_PORT, &src, &dst).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Target folder");
                            ui.text_edit_singleline(&mut self.manage_target_dir);
                            let can_move = can_ops;
                            if ui.add_enabled(can_move, egui::Button::new("Move")).clicked() {
                                let src = selected_path.clone();
                                let target_dir = if self.manage_target_dir.trim().is_empty() {
                                    self.manage_path.clone()
                                } else {
                                    self.manage_target_dir.clone()
                                };
                                let dst = Self::join_remote_path(&target_dir, &selected_name);
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("Move", move || {
                                    rt.block_on(async {
                                        move_path(&ip, TRANSFER_PORT, &src, &dst).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                        });

                        ui.horizontal(|ui| {
                            let can_copy = can_ops;
                            if ui.add_enabled(can_copy, egui::Button::new("Copy")).clicked() {
                                self.manage_clipboard = Some(selected_path.clone());
                                self.manage_status = "Copied to clipboard".to_string();
                            }

                            let can_paste = can_ops && self.manage_clipboard.is_some();
                            if ui.add_enabled(can_paste, egui::Button::new("Paste")).clicked() {
                                if let Some(clip_path) = self.manage_clipboard.clone() {
                                    let name = std::path::Path::new(&clip_path)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "item".to_string());
                                    let dst = Self::join_remote_path(&self.manage_path, &name);
                                    let ip = self.ip.clone();
                                    let rt = self.rt.clone();
                                    self.manage_send_op("Paste", move || {
                                        rt.block_on(async {
                                            copy_path(&ip, TRANSFER_PORT, &clip_path, &dst).await
                                        }).map_err(|e| e.to_string())
                                    });
                                }
                            }
                        });

                        ui.horizontal(|ui| {
                            let is_file = selected_type == "file";
                            let can_download = can_ops && is_file;
                            if ui.add_enabled(can_download, egui::Button::new("Download")).clicked() {
                                let default_name = selected_name.clone();
                                if let Some(save_path) = rfd::FileDialog::new().set_file_name(&default_name).save_file() {
                                    let target = selected_path.clone();
                                    let ip = self.ip.clone();
                                    let rt = self.rt.clone();
                                    let save_path = save_path.display().to_string();
                                    self.manage_send_op("Download", move || {
                                        rt.block_on(async {
                                            download_file(&ip, TRANSFER_PORT, &target, &save_path).await
                                        }).map_err(|e| e.to_string())
                                    });
                                }
                            }
                            let can_delete = can_ops;
                            if ui.add_enabled(can_delete, egui::Button::new("Delete")).clicked() {
                                let target = selected_path.clone();
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("Delete", move || {
                                    rt.block_on(async {
                                        delete_path(&ip, TRANSFER_PORT, &target).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                            let can_chmod = can_ops;
                            if ui.add_enabled(can_chmod, egui::Button::new("chmod 777"))
                                .on_hover_text("Applies recursively")
                                .clicked() {
                                let target = selected_path.clone();
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("chmod", move || {
                                    rt.block_on(async {
                                        chmod_777(&ip, TRANSFER_PORT, &target).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                        });
                    } else {
                        ui.label("Select an item to manage.");
                    }
                });
            } else {
                // Drag & Drop handling
                let drag_active = ctx.input(|i| !i.raw.hovered_files.is_empty());
                let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());

                if drag_active {
                    let rect = ui.available_rect_before_wrap();
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(0, 102, 204, 40),
                    );
                    ui.painter().rect_stroke(
                        rect,
                        8.0,
                        egui::Stroke::new(3.0, egui::Color32::from_rgb(0, 102, 204)),
                    );
                    ui.centered_and_justified(|ui| {
                        ui.heading("Drop folder here");
                    });
                }

                if let Some(file) = dropped_files.first() {
                    if let Some(path) = &file.path {
                        if path.is_dir() {
                            self.update_game_path(path.display().to_string());
                        } else if let Some(parent) = path.parent() {
                            self.update_game_path(parent.display().to_string());
                        }
                    }
                }

                ui.add_space(10.0);
                ui.heading("Transfer");
                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Source Folder");
                    ui.horizontal(|ui| {
                        if ui.button("📂 Browse").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                self.update_game_path(path.display().to_string());
                            }
                        }
                        let can_add_queue = !self.game_path.trim().is_empty() && !self.is_uploading;
                        if ui.add_enabled(can_add_queue, egui::Button::new("+ Queue")).on_hover_text("Add to transfer queue").clicked() {
                            self.add_to_queue();
                        }
                        ui.text_edit_singleline(&mut self.game_path);
                    });
                    if self.calculating_size { ui.spinner(); ui.label("Measuring..."); }
                    else if let Some(size) = self.calculated_size { ui.label(format!("Total Size: {}", format_bytes(size))); }
                });

                // Transfer Queue (collapsible)
                let pending_count = self.queue_data.items.iter().filter(|i| i.status == QueueStatus::Pending).count();
                let queue_label = if pending_count > 0 {
                    format!("Transfer Queue ({})", pending_count)
                } else {
                    "Transfer Queue".to_string()
                };

                ui.add_space(5.0);
                egui::CollapsingHeader::new(queue_label)
                    .default_open(false)
                    .show(ui, |ui| {
                        if self.queue_data.items.is_empty() {
                            ui.label("Queue is empty. Use '+ Queue' to add items.");
                        } else {
                            let mut item_to_remove: Option<u64> = None;

                            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                                for item in &self.queue_data.items {
                                    ui.horizontal(|ui| {
                                        let (icon, color) = match &item.status {
                                            QueueStatus::Pending => ("○", egui::Color32::GRAY),
                                            QueueStatus::InProgress => ("●", egui::Color32::from_rgb(0, 102, 204)),
                                            QueueStatus::Completed => ("✓", egui::Color32::from_rgb(100, 200, 100)),
                                            QueueStatus::Failed(_) => ("✗", egui::Color32::from_rgb(200, 100, 100)),
                                        };
                                        ui.label(egui::RichText::new(icon).color(color));
                                        ui.label(&item.subfolder_name);

                                        if let Some(size) = item.size_bytes {
                                            ui.label(egui::RichText::new(format_bytes(size)).weak());
                                        }

                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if item.status == QueueStatus::Pending {
                                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                                    item_to_remove = Some(item.id);
                                                }
                                            }
                                        });
                                    });
                                }
                            });

                            if let Some(id) = item_to_remove {
                                self.remove_from_queue(id);
                            }

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                let has_pending = self.queue_data.items.iter().any(|i| i.status == QueueStatus::Pending);
                                let can_start = has_pending && !self.is_uploading && connected;
                                if ui.add_enabled(can_start, egui::Button::new("▶ Start Queue")).clicked() {
                                    self.process_next_queue_item();
                                }
                                if ui.button("Clear Completed").clicked() {
                                    self.clear_completed_queue();
                                }
                            });
                        }
                    });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Destination Path");
                    egui::Grid::new("dest_grid").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
                        ui.label("Preset:");
                        egui::ComboBox::from_id_source("preset_combo").selected_text(PRESETS[self.selected_preset]).show_ui(ui, |ui| {
                            for (i, p) in PRESETS.iter().enumerate() { ui.selectable_value(&mut self.selected_preset, i, *p); }
                        });
                        ui.end_row();
                        if self.selected_preset == 2 { ui.label("Path:"); ui.text_edit_singleline(&mut self.custom_preset_path); ui.end_row(); }
                        ui.label("Name:"); ui.text_edit_singleline(&mut self.custom_subfolder); ui.end_row();
                        ui.label("Use Temp:");
                        let temp_toggle = ui.checkbox(&mut self.config.use_temp, "Stage on fastest storage");
                        if temp_toggle.changed() { self.config.save(); }
                        ui.end_row();
                    });
                    ui.add_space(5.0);
                    ui.label(egui::RichText::new(format!("➡ Destination: {}", self.get_dest_path())).monospace().weak());
                });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Upload Progress");
                    ui.add_space(5.0);
                    
                    let total = self.progress_total.max(1);
                    let progress = (self.progress_sent as f64 / total as f64).clamp(0.0, 1.0) as f32;
                    ui.add(egui::ProgressBar::new(progress).show_percentage().animate(self.is_uploading));

                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.label(format!("{} / {}", format_bytes(self.progress_sent), format_bytes(self.progress_total)));
                        ui.separator();
                        ui.label(format!("{}/s", format_bytes(self.progress_speed_bps as u64)));
                        ui.separator();
                        ui.label(format!("ETA {}", self.progress_eta_secs.map(format_duration).unwrap_or("N/A".to_string())));
                    });
                    
                    if self.progress_files > 0 { ui.label(format!("Files transferred: {}", self.progress_files)); }
                    if !self.progress_current_file.is_empty() {
                        ui.label(format!("Current file: {}", self.progress_current_file));
                    }
                    
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.label("Connections");
                        let mut connections = self.config.connections as i32;
                        let response = ui.add(
                            egui::DragValue::new(&mut connections)
                                .clamp_range(1..=MAX_PARALLEL_CONNECTIONS as i32)
                                .speed(1.0)
                        );
                        if response.changed() {
                            self.config.connections = connections as usize;
                            self.config.save();
                        }
                        ui.label(format!("(max {})", MAX_PARALLEL_CONNECTIONS));
                    });

                    if self.is_uploading {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Uploading...");
                            if ui.add(egui::Button::new("❌ Stop Upload").min_size([100.0, 32.0].into())).clicked() {
                                self.upload_cancellation_token.store(true, Ordering::Relaxed);
                                self.log("Stopping...");
                            }
                        });
                    } else {
                        ui.add_space(15.0);
                        let enabled = !self.game_path.is_empty() && self.selected_storage.is_some() && connected;
                        if ui.add_enabled(enabled, egui::Button::new("🚀 Start Upload").min_size([ui.available_width(), 40.0].into())).clicked() {
                            self.check_exists_and_upload();
                        }
                    }
                });
            }
        });
    }
}

async fn fetch_latest_release() -> anyhow::Result<ReleaseInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/repos/phantomptr/ps5upload/releases/latest")
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;

    let release = response.json::<ReleaseInfo>().await?;
    Ok(release)
}

async fn download_asset(url: &str, dest_path: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;

    let bytes = response.bytes().await?;
    std::fs::write(dest_path, bytes)?;
    Ok(())
}

fn normalize_version(version: &str) -> String {
    version.trim_start_matches('v').trim().to_string()
}

fn is_newer_version(latest: &str, current: &str) -> Option<bool> {
    let latest_norm = normalize_version(latest);
    let current_norm = normalize_version(current);
    let latest_v = semver::Version::parse(&latest_norm).ok()?;
    let current_v = semver::Version::parse(&current_norm).ok()?;
    Some(latest_v > current_v)
}

fn current_asset_name() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let arch_name = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return Err(format!("Unsupported arch: {}", arch)),
    };

    let os_name = match os {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => return Err(format!("Unsupported OS: {}", os)),
    };

    Ok(format!("ps5upload-{}-{}.zip", os_name, arch_name))
}

fn setup_custom_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_rounding = 8.0.into();
    visuals.selection.bg_fill = egui::Color32::from_rgb(0, 102, 204);
    visuals.widgets.noninteractive.bg_stroke.color = egui::Color32::from_gray(60);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_gray(220);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_gray(30);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::new(14.0, egui::FontFamily::Proportional));
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(20.0, egui::FontFamily::Proportional));
    style.spacing.item_spacing = [8.0, 8.0].into();
    style.spacing.button_padding = [10.0, 6.0].into();
    ctx.set_style(style);
}

fn setup_light_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.window_rounding = 8.0.into();
    visuals.selection.bg_fill = egui::Color32::from_rgb(0, 102, 204);
    visuals.widgets.noninteractive.bg_stroke.color = egui::Color32::from_gray(180);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_gray(40);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_gray(230);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::new(14.0, egui::FontFamily::Proportional));
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(20.0, egui::FontFamily::Proportional));
    style.spacing.item_spacing = [8.0, 8.0].into();
    style.spacing.button_padding = [10.0, 6.0].into();
    ctx.set_style(style);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([1200.0, 800.0])
            .with_icon(std::sync::Arc::new(build_icon())),
        ..Default::default()
    };
    eframe::run_native("PS5 Upload", options, Box::new(|cc| Box::new(Ps5UploadApp::new(cc))))
}

fn build_icon() -> egui::IconData {
    let width = 32;
    let height = 32;
    let mut rgba = vec![0u8; width * height * 4];
    let bg = [20u8, 20u8, 24u8, 255u8];
    let fg = [98u8, 161u8, 255u8, 255u8];
    for y in 0..height { for x in 0..width { let idx = (y * width + x) * 4; rgba[idx..idx + 4].copy_from_slice(&bg); } }
    for y in 6..26 { for x in 8..11 { let idx = (y * width + x) * 4; rgba[idx..idx + 4].copy_from_slice(&fg); } }
    for y in 6..12 { for x in 11..20 { let idx = (y * width + x) * 4; rgba[idx..idx + 4].copy_from_slice(&fg); } }
    for y in 12..18 { for x in 18..21 { let idx = (y * width + x) * 4; rgba[idx..idx + 4].copy_from_slice(&fg); } }
    for y in 18..24 { for x in 11..20 { let idx = (y * width + x) * 4; rgba[idx..idx + 4].copy_from_slice(&fg); } }
    egui::IconData { rgba, width: width as u32, height: height as u32 }
}

fn format_modified_time(mtime: Option<i64>) -> String {
    let Some(ts) = mtime else {
        return "--".to_string();
    };

    let now = chrono::Utc::now().timestamp();
    let delta = now.saturating_sub(ts);
    if delta < 60 {
        return "just now".to_string();
    }
    if delta < 3600 {
        return format!("{}m ago", delta / 60);
    }
    if delta < 86400 {
        return format!("{}h ago", delta / 3600);
    }
    if delta < 86400 * 7 {
        return format!("{}d ago", delta / 86400);
    }

    let Some(dt_utc) = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0) else {
        return "--".to_string();
    };
    let dt_local = dt_utc.with_timezone(&chrono::Local);
    dt_local.format("%Y-%m-%d %H:%M").to_string()
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0; const MB: f64 = 1024.0 * KB; const GB: f64 = 1024.0 * MB;
    let b = bytes as f64;
    if b >= GB { format!("{:.2} GB", b / GB) }
    else if b >= MB { format!("{:.2} MB", b / MB) }
    else if b >= KB { format!("{:.2} KB", b / KB) }
    else { format!("{} B", bytes) }
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 { return "N/A".to_string(); }
    let total = seconds.round() as u64;
    let mins = total / 60; let secs = total % 60;
    if mins > 0 { format!("{}m {}s", mins, secs) } else { format!("{}s", secs) }
}

fn parse_upload_response(response: &str) -> anyhow::Result<(i32, u64)> {
    if response.starts_with("SUCCESS") {
        let parts: Vec<&str> = response.split_whitespace().collect();
        let files = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let bytes = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok((files, bytes))
    } else {
        Err(anyhow::anyhow!("Upload failed: {}", response))
    }
}

fn partition_files_by_size(mut files: Vec<FileEntry>, connections: usize) -> Vec<Vec<FileEntry>> {
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    let mut buckets: Vec<Vec<FileEntry>> = vec![Vec::new(); connections];
    let mut bucket_sizes = vec![0u64; connections];

    for file in files {
        let (idx, _) = bucket_sizes
            .iter()
            .enumerate()
            .min_by_key(|(_, size)| *size)
            .unwrap();
        bucket_sizes[idx] += file.size;
        buckets[idx].push(file);
    }

    buckets
}
