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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::runtime::Runtime;

mod protocol;
mod archive;
mod transfer;
mod config;

use protocol::{StorageLocation, list_storage, check_dir, upload_v2_init};
use archive::get_size;
use transfer::{collect_files, send_files_v2_for_list, FileEntry};
use config::AppConfig;

const PRESETS: [&str; 3] = ["etaHEN/games", "homebrew", "custom"];
const TRANSFER_PORT: u16 = 9113;
const PAYLOAD_PORT: u16 = 9021;
const MAX_PARALLEL_CONNECTIONS: usize = 12;

enum AppMessage {
    Log(String),
    PayloadLog(String),
    PayloadSendComplete(Result<u64, String>),
    StorageList(Result<Vec<StorageLocation>, String>),
    CheckExistsResult(bool),
    SizeCalculated(u64),
    UploadStart,
    Progress { sent: u64, total: u64, files_sent: i32, elapsed_secs: f64, current_file: Option<String> },
    UploadComplete(Result<(i32, u64), String>),
}

struct Ps5UploadApp {
    // UI State
    ip: String,
    
    // Source
    game_path: String,
    
    // Destination
    selected_storage: Option<String>,
    selected_preset: usize,
    custom_preset_path: String,
    custom_subfolder: String, // Calculated from game_path usually

    storage_locations: Vec<StorageLocation>,
    
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
    log_tab: usize, // 0 = Client, 1 = Payload

    // Concurrency
    rx: Receiver<AppMessage>,
    tx: Sender<AppMessage>,
    rt: Arc<Runtime>,
    
    // Config
    config: AppConfig,
}

impl Ps5UploadApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        let (tx, rx) = channel();
        let rt = Runtime::new().expect("Failed to create Tokio runtime");
        
        setup_custom_style(&cc.egui_ctx);
        
        let config = AppConfig::load();

        Self {
            ip: config.address.clone(),
            game_path: String::new(),
            selected_storage: Some(config.storage.clone()),
            selected_preset: 0,
            custom_preset_path: String::new(),
            custom_subfolder: String::new(),
            storage_locations: Vec::new(),
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
            rx,
            tx,
            rt: Arc::new(rt),
            config,
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

        let ip = self.ip.clone();
        let path = self.payload_path.clone();
        let tx = self.tx.clone();

        thread::spawn(move || {
            use std::fs::File;
            use std::net::TcpStream;

            let result = (|| -> Result<u64, String> {
                let mut file = File::open(&path)
                    .map_err(|e| format!("Failed to open payload: {}", e))?;
                let mut stream = TcpStream::connect((ip.as_str(), PAYLOAD_PORT))
                    .map_err(|e| format!("Failed to connect: {}", e))?;
                let _ = stream.set_nodelay(true);
                let bytes = std::io::copy(&mut file, &mut stream)
                    .map_err(|e| format!("Send failed: {}", e))?;
                Ok(bytes)
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
                            }
                            self.status = "Connected".to_string();
                            self.log("Connected to PS5");
                            self.payload_log("Connected and Storage scanned.");
                        }
                        Err(e) => {
                            self.log(&format!("Error: {}", e));
                            self.status = "Connection Failed".to_string();
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
                    match res {
                        Ok((files, bytes)) => {
                            self.progress_files = files;
                            self.log(&format!("✓ Completed: {} files, {}", files, format_bytes(bytes)));
                            self.payload_log("SUCCESS");
                            self.status = "Upload Complete!".to_string();
                            if self.progress_total > 0 { self.progress_sent = self.progress_total; }
                            if self.config.auto_connect {
                                self.connect();
                            }
                        }
                        Err(e) => {
                            self.log(&format!("✗ Failed: {}", e));
                            self.payload_log(&format!("ERROR: {}", e));
                            self.status = "Upload Failed".to_string();
                            if self.config.auto_connect && self.upload_cancellation_token.load(Ordering::Relaxed) {
                                self.connect();
                            }
                        }
                    }
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
            });
            ui.separator();

            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                let text = if self.log_tab == 0 { &self.client_logs } else { &self.payload_logs };
                ui.add(egui::TextEdit::multiline(&mut text.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .lock_focus(true));
            });
        });

        // 5. CENTRAL PANEL: TRANSFER (Last)
        egui::CentralPanel::default().show(ctx, |ui| {
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
                    ui.text_edit_singleline(&mut self.game_path);
                });
                if self.calculating_size { ui.spinner(); ui.label("Measuring..."); }
                else if let Some(size) = self.calculated_size { ui.label(format!("Total Size: {}", format_bytes(size))); }
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
        });
    }
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([900.0, 600.0])
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
