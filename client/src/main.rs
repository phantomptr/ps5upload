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
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, AtomicU64, Ordering};
use std::io::{Read, Write};
use tokio::runtime::Runtime;
use serde::Deserialize;
use lz4_flex::block::compress_prepend_size;
use chat::ChatMessage;
use rand::Rng;
use hex;

mod protocol;
mod archive;
mod transfer;
mod config;
mod profiles;
mod history;
mod queue;
mod i18n;
mod chat;

const CHAT_MAX_MESSAGES: usize = 500;

use protocol::{StorageLocation, DirEntry, DownloadCompression, list_storage, list_dir, list_dir_recursive, check_dir, upload_v2_init, delete_path, move_path, copy_path, chmod_777, create_path, download_file_with_progress, download_dir_with_progress, get_payload_version, hash_file, upload_rar_for_extraction, get_space};
use archive::get_size;
use transfer::{collect_files_with_progress, stream_files_with_progress, send_files_v2_for_list, SendFilesConfig, scan_zip_archive, send_zip_archive, scan_7z_archive, send_7z_archive, SharedReceiverIterator, FileEntry, CompressionMode};
use config::AppConfig;
use profiles::{Profile, ProfilesData, load_profiles, save_profiles};
use history::{TransferRecord, HistoryData, load_history, add_record, clear_history};
use queue::{QueueItem, QueueData, QueueStatus, load_queue, save_queue};
use i18n::{Language, tr};
use sha2::{Digest, Sha256};
use image::GenericImageView;
use walkdir::WalkDir;

const LOGO_BYTES: &[u8] = include_bytes!("../../logo.png");

const PRESETS: [&str; 3] = ["etaHEN/games", "homebrew", "custom"];
const TRANSFER_PORT: u16 = 9113;
const PAYLOAD_PORT: u16 = 9021;
const MAX_PARALLEL_CONNECTIONS: usize = 10; // Increased back to 10 for better saturation
const MAX_LOG_BYTES: usize = 512 * 1024;


#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileSnapshot {
    address: String,
    storage: String,
    preset_index: usize,
    custom_preset_path: String,
    connections: usize,
    use_temp: bool,
    auto_tune_connections: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManageSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManageDestAction {
    Move,
    Copy,
}

enum ChatStatusEvent {
    Connected,
    Disconnected,
}

enum AppMessage {
    Log(String),
    PayloadLog(String),
    StatusPhase(String),
    ChatMessage(ChatMessage),
    ChatStatus(ChatStatusEvent),
    ChatAck { ok: bool, reason: Option<String> },
    PayloadSendComplete(Result<u64, String>),
    PayloadVersion(Result<String, String>),
    StorageList(Result<Vec<StorageLocation>, String>),
    ManageList { side: ManageSide, result: Result<Vec<DirEntry>, String> },
    ManageOpComplete { op: String, result: Result<(), String> },
    DownloadStart { total: u64, label: String },
    DownloadProgress { received: u64, total: u64, current_file: Option<String> },
    DownloadComplete(Result<u64, String>),
    UploadOptimizeComplete(UploadOptimization),
    MoveCheckResult { req: MoveRequest, exists: bool },
    UpdateCheckComplete(Result<ReleaseInfo, String>),
    UpdateDownloadComplete { kind: String, result: Result<String, String> },
    SelfUpdateReady(Result<PendingUpdate, String>),
    CheckExistsResult(bool),
    SizeCalculated(u64),
    Scanning { run_id: u64, files_found: usize, total_size: u64 },
    UploadStart { run_id: u64 },
    Progress { run_id: u64, sent: u64, total: u64, files_sent: i32, elapsed_secs: f64, current_file: Option<String> },
    UploadComplete { run_id: u64, result: Result<(i32, u64), String> },
}

const APP_VERSION: &str = include_str!("../../VERSION");
include!(concat!(env!("OUT_DIR"), "/chat_key.rs"));

fn app_version_trimmed() -> &'static str {
    APP_VERSION.trim()
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
    prerelease: bool,
}

#[derive(Clone, Debug)]
struct PendingUpdate {
    target_path: std::path::PathBuf,
    replacement_path: std::path::PathBuf,
    restart_path: std::path::PathBuf,
    is_dir: bool,
}

#[derive(Clone)]
enum DownloadRequest {
    File { name: String, target: String, save_path: String },
    Dir { name: String, target: String, dest_root: String, compression: protocol::DownloadCompression },
}

#[derive(Clone, Copy)]
enum PayloadFetch {
    Current,
    Latest,
}

#[derive(Clone)]
struct MoveRequest {
    src: String,
    dst: String,
    op_name: String,
    dst_exists: bool,
}

#[derive(Clone)]
struct RenameRequest {
    src: String,
    dst: String,
}

#[derive(Clone, Debug)]
struct UploadOptimization {
    compression: Option<CompressionMode>,
    connections: Option<usize>,
    sample_files: Option<usize>,
    sample_bytes: Option<u64>,
}

struct Ps5UploadApp {
    // UI State
    ip: String,
    main_tab: usize, // 0 = Transfer, 1 = Manage, 2 = Chat
    
    // Source
    game_path: String,
    
    // Destination
    selected_storage: Option<String>,
    selected_preset: usize,
    custom_preset_path: String,
    custom_subfolder: String, // Calculated from game_path usually

    storage_locations: Vec<StorageLocation>,

    // Manage
    manage_left_path: String,
    manage_right_path: String,
    manage_left_entries: Vec<DirEntry>,
    manage_right_entries: Vec<DirEntry>,
    manage_left_selected: Option<usize>,
    manage_right_selected: Option<usize>,
    manage_left_status: String,
    manage_right_status: String,
    manage_new_name: String,
    manage_busy: bool,
    manage_dest_open: bool,
    manage_dest_action: Option<ManageDestAction>,
    manage_dest_source_path: Option<String>,
    manage_dest_source_name: Option<String>,

    // Download
    is_downloading: bool,
    download_cancellation_token: Arc<AtomicBool>,
    download_progress_sent: u64,
    download_progress_total: u64,
    download_speed_bps: f64,
    download_eta_secs: Option<f64>,
    download_current_file: String,

    // Move
    is_moving: bool,
    move_cancellation_token: Arc<AtomicBool>,

    // Updates
    update_info: Option<ReleaseInfo>,
    update_status: String,
    update_available: bool,
    update_check_running: bool,
    update_download_status: String,
    pending_update: Option<PendingUpdate>,
    
    client_logs: String,
    payload_logs: String,
    status: String,
    is_uploading: bool,
    is_connecting: bool,
    is_sending_payload: bool,
    is_connected: bool,
    
    // Cancellation
    upload_cancellation_token: Arc<AtomicBool>,
    
    // Override Dialog
    show_override_dialog: bool,
    show_resume_dialog: bool,
    force_full_upload_once: bool,
    show_download_overwrite_dialog: bool,
    pending_download_request: Option<DownloadRequest>,
    show_move_overwrite_dialog: bool,
    pending_move_request: Option<MoveRequest>,
    show_delete_confirm: bool,
    pending_delete_target: Option<String>,
    show_rename_confirm: bool,
    pending_rename_request: Option<RenameRequest>,
    show_archive_confirm_dialog: bool,
    pending_archive_path: Option<String>,
    pending_archive_kind: Option<String>,
    pending_archive_trim: bool,
    show_update_restart_dialog: bool,
    
    // Progress
    progress_sent: u64,
    progress_total: u64,
    progress_speed_bps: f64,
    progress_eta_secs: Option<f64>,
    progress_files: i32,
    progress_current_file: String,
    progress_phase: String,
    upload_run_id: u64,

    // Scanning state
    is_scanning: bool,
    scanning_files_found: usize,
    scanning_total_size: u64,

    // Size Calc
    calculating_size: bool,
    calculated_size: Option<u64>,
    
    // Payload
    payload_path: String,
    payload_status: String,
    payload_version: Option<String>,

    // UI Toggles
    log_tab: usize, // 0 = Client, 1 = Payload, 2 = History
    theme_dark: bool,
    language: Language,
    logo_texture: Option<egui::TextureHandle>,

    // Chat
    chat_messages: Vec<ChatMessage>,
    chat_input: String,
    chat_status: String,
    chat_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    chat_sent_count: u64,
    chat_received_count: u64,
    chat_ack_count: u64,
    chat_reject_count: u64,
    chat_room_id: String,

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
    profile_dirty: bool,
    last_profile_snapshot: Option<ProfileSnapshot>,
    last_profile_change: Option<std::time::Instant>,

    // History
    history_data: HistoryData,
    upload_start_time: Option<std::time::Instant>,
    upload_source_path: String,
    upload_dest_path: String,
    download_start_time: Option<std::time::Instant>,
    show_history_resume_dialog: bool,
    pending_history_record: Option<TransferRecord>,
    history_resume_mode: String,
    auto_resume_on_exists: bool,

    // Queue
    queue_data: QueueData,
    current_queue_item_id: Option<u64>,

    // Auto-connect
    last_auto_connect_attempt: Option<std::time::Instant>,
    last_payload_check: Option<std::time::Instant>,
    
    // Internal
    forced_dest_path: Option<String>,
    show_archive_overwrite_dialog: bool,
    archive_overwrite_confirmed: bool,
}

impl Ps5UploadApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        let (tx, rx) = channel();
        let rt = Runtime::new().expect("Failed to create Tokio runtime");

        let config = AppConfig::load();
        let theme_dark = config.theme != "light";
        let language = Language::from_code(&config.language);
        let profiles_data = load_profiles();
        let history_data = load_history();
        let queue_data = load_queue();

        // Setup fonts for CJK and Arabic language support
        setup_fonts(&cc.egui_ctx);

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
            manage_left_path: "/data".to_string(),
            manage_right_path: "/data".to_string(),
            manage_left_entries: Vec::new(),
            manage_right_entries: Vec::new(),
            manage_left_selected: None,
            manage_right_selected: None,
            manage_left_status: "Not connected".to_string(),
            manage_right_status: "Not connected".to_string(),
            manage_new_name: String::new(),
            manage_busy: false,
            manage_dest_open: false,
            manage_dest_action: None,
            manage_dest_source_path: None,
            manage_dest_source_name: None,
            is_downloading: false,
            download_cancellation_token: Arc::new(AtomicBool::new(false)),
            download_progress_sent: 0,
            download_progress_total: 0,
            download_speed_bps: 0.0,
            download_eta_secs: None,
            download_current_file: String::new(),
            is_moving: false,
            move_cancellation_token: Arc::new(AtomicBool::new(false)),
            update_info: None,
            update_status: "Checking for updates...".to_string(),
            update_available: false,
            update_check_running: false,
            update_download_status: String::new(),
            pending_update: None,
            client_logs: String::new(),
            payload_logs: String::new(),
            status: "Ready".to_string(),
            is_uploading: false,
            is_connecting: false,
            is_sending_payload: false,
            is_connected: false,
            upload_cancellation_token: Arc::new(AtomicBool::new(false)),
            show_override_dialog: false,
            show_resume_dialog: false,
            force_full_upload_once: false,
            show_download_overwrite_dialog: false,
            pending_download_request: None,
            show_move_overwrite_dialog: false,
            pending_move_request: None,
            show_delete_confirm: false,
            pending_delete_target: None,
            show_rename_confirm: false,
            pending_rename_request: None,
            show_archive_confirm_dialog: false,
            pending_archive_path: None,
            pending_archive_kind: None,
            pending_archive_trim: true,
            show_update_restart_dialog: false,
            progress_sent: 0,
            progress_total: 0,
            progress_speed_bps: 0.0,
            progress_eta_secs: None,
            progress_files: 0,
            progress_current_file: String::new(),
            progress_phase: String::new(),
            upload_run_id: 0,
            is_scanning: false,
            scanning_files_found: 0,
            scanning_total_size: 0,
            calculating_size: false,
            calculated_size: None,
            payload_path: String::new(),
            payload_status: "Unknown (not checked)".to_string(),
            payload_version: None,
            log_tab: 0,
            theme_dark,
            language,
            logo_texture: None,
            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_status: tr(language, "chat_connecting"),
            chat_tx: None,
            chat_sent_count: 0,
            chat_received_count: 0,
            chat_ack_count: 0,
            chat_reject_count: 0,
            chat_room_id: String::new(),
            rx,
            tx,
            rt: Arc::new(rt),
            config,
            profiles_data,
            current_profile: None,
            show_profile_dialog: false,
            editing_profile: None,
            profile_name_input: String::new(),
            profile_dirty: false,
            last_profile_snapshot: None,
            last_profile_change: None,
            history_data,
            upload_start_time: None,
            upload_source_path: String::new(),
            upload_dest_path: String::new(),
            download_start_time: None,
            show_history_resume_dialog: false,
            pending_history_record: None,
            history_resume_mode: "size".to_string(),
            auto_resume_on_exists: false,
            queue_data,
            current_queue_item_id: None,
            last_auto_connect_attempt: None,
            last_payload_check: None,
            forced_dest_path: None,
            show_archive_overwrite_dialog: false,
            archive_overwrite_confirmed: true,
        };

        app.ensure_chat_display_name();

        if let Some(default_name) = app.profiles_data.default_profile.clone() {
            if let Some(profile) = app.profiles_data.profiles.iter().find(|p| p.name == default_name).cloned() {
                app.apply_profile(&profile);
            }
        }

        app.start_chat();
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
        let _ = self.config.save();
    }

    fn apply_profile(&mut self, profile: &Profile) {
        self.ip = profile.address.clone();
        self.selected_storage = Some(profile.storage.clone());
        self.selected_preset = profile.preset_index;
        self.custom_preset_path = profile.custom_preset_path.clone();
        self.config.connections = profile.connections;
        self.config.use_temp = profile.use_temp;
        self.config.auto_tune_connections = profile.auto_tune_connections;
        self.config.address = profile.address.clone();
        self.config.storage = profile.storage.clone();
        let _ = self.config.save();
        self.current_profile = Some(profile.name.clone());
    }

    fn set_default_profile(&mut self, name: Option<String>) {
        self.profiles_data.default_profile = name;
        save_profiles(&self.profiles_data);
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
            auto_tune_connections: self.config.auto_tune_connections,
        };

        // Update or add profile
        if let Some(existing) = self.profiles_data.profiles.iter_mut().find(|p| p.name == name) {
            *existing = profile;
        } else {
            self.profiles_data.profiles.push(profile);
        }
        save_profiles(&self.profiles_data);
        self.current_profile = Some(name);
        self.set_default_profile(self.current_profile.clone());
    }

    fn current_profile_snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            address: self.ip.clone(),
            storage: self.selected_storage.clone().unwrap_or_else(|| "/data".to_string()),
            preset_index: self.selected_preset,
            custom_preset_path: self.custom_preset_path.clone(),
            connections: self.config.connections,
            use_temp: self.config.use_temp,
            auto_tune_connections: self.config.auto_tune_connections,
        }
    }

    fn profile_matches_snapshot(profile: &Profile, snap: &ProfileSnapshot) -> bool {
        profile.address == snap.address
            && profile.storage == snap.storage
            && profile.preset_index == snap.preset_index
            && profile.custom_preset_path == snap.custom_preset_path
            && profile.connections == snap.connections
            && profile.use_temp == snap.use_temp
            && profile.auto_tune_connections == snap.auto_tune_connections
    }

    fn autosave_profile_if_needed(&mut self) {
        let Some(name) = self.current_profile.clone() else {
            self.profile_dirty = false;
            self.last_profile_snapshot = None;
            self.last_profile_change = None;
            return;
        };
        let Some(profile) = self.profiles_data.profiles.iter().find(|p| p.name == name) else {
            return;
        };

        let snapshot = self.current_profile_snapshot();
        let snapshot_changed = self.last_profile_snapshot.as_ref().map(|s| s != &snapshot).unwrap_or(true);
        if snapshot_changed {
            self.last_profile_snapshot = Some(snapshot.clone());
            self.last_profile_change = Some(std::time::Instant::now());
        }

        if Self::profile_matches_snapshot(profile, &snapshot) {
            self.profile_dirty = false;
            self.last_profile_change = None;
            return;
        }

        let now = std::time::Instant::now();
        let change_time = self.last_profile_change.get_or_insert(now);
        if now.duration_since(*change_time) >= std::time::Duration::from_secs(1) {
            self.save_current_as_profile(name);
            self.profile_dirty = false;
            self.last_profile_change = None;
        } else {
            self.profile_dirty = true;
        }
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

        let dest_path = self.get_dest_path();

        let item = QueueItem {
            id: self.queue_data.next_id,
            source_path: self.game_path.clone(),
            subfolder_name: subfolder,
            preset_index: self.selected_preset,
            custom_preset_path: self.custom_preset_path.clone(),
            storage_base: self.selected_storage.clone().unwrap_or_else(|| self.config.storage.clone()),
            dest_path,
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
            if !item_clone.storage_base.is_empty() {
                self.selected_storage = Some(item_clone.storage_base.clone());
            }
            if !item_clone.dest_path.is_empty() {
                self.forced_dest_path = Some(item_clone.dest_path.clone());
            } else {
                self.forced_dest_path = None;
            }
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
        self.trim_logs();
    }

    fn log_peak_rss(&mut self, label: &str) {
        #[cfg(unix)]
        {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            let res = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
            if res == 0 {
                let usage = unsafe { usage.assume_init() };
                let rss_kb = usage.ru_maxrss as u64;
                #[cfg(target_os = "macos")]
                let rss_kb = rss_kb / 1024;
                self.log(&format!("Peak RSS ({}): {} MB", label, rss_kb / 1024));
            }
        }
    }

    fn payload_log(&mut self, msg: &str) {
        let time = chrono::Local::now().format("%H:%M:%S");
        self.payload_logs.push_str(&format!("[{}] {}\n", time, msg));
        self.trim_logs();
    }

    fn trim_logs(&mut self) {
        if self.client_logs.len() > MAX_LOG_BYTES {
            let start = self.client_logs.len() - MAX_LOG_BYTES;
            let cut = self.client_logs[start..].find('\n').map(|idx| start + idx + 1).unwrap_or(start);
            self.client_logs.drain(..cut);
        }
        if self.payload_logs.len() > MAX_LOG_BYTES {
            let start = self.payload_logs.len() - MAX_LOG_BYTES;
            let cut = self.payload_logs[start..].find('\n').map(|idx| start + idx + 1).unwrap_or(start);
            self.payload_logs.drain(..cut);
        }
    }

    fn push_chat_message(&mut self, msg: ChatMessage) {
        self.chat_messages.push(msg);
        if self.chat_messages.len() > CHAT_MAX_MESSAGES {
            let remove_count = self.chat_messages.len() - CHAT_MAX_MESSAGES;
            self.chat_messages.drain(0..remove_count);
        }
    }

    fn join_remote_path(base: &str, name: &str) -> String {
        if base.ends_with('/') {
            format!("{}{}", base, name)
        } else {
            format!("{}/{}", base, name)
        }
    }

    fn manage_refresh(&mut self, side: ManageSide) {
        let path = match side {
            ManageSide::Left => self.manage_left_path.clone(),
            ManageSide::Right => self.manage_right_path.clone(),
        };
        if self.ip.trim().is_empty() || path.trim().is_empty() {
            return;
        }

        self.manage_busy = true;
        match side {
            ManageSide::Left => self.manage_left_status = "Listing...".to_string(),
            ManageSide::Right => self.manage_right_status = "Listing...".to_string(),
        }
        self.log(&format!("Listing {}", path));
        let ip = self.ip.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();

        thread::spawn(move || {
            let res = rt.block_on(async { list_dir(&ip, TRANSFER_PORT, &path).await });
            let _ = tx.send(AppMessage::ManageList { side, result: res.map_err(|e| e.to_string()) });
        });
    }

    fn open_destination_picker(&mut self, action: ManageDestAction, src_path: String, src_name: String) {
        self.manage_dest_action = Some(action);
        self.manage_dest_source_path = Some(src_path);
        self.manage_dest_source_name = Some(src_name);
        if self.manage_right_path.trim().is_empty() {
            self.manage_right_path = self.selected_storage.clone().unwrap_or_else(|| "/data".to_string());
        }
        self.manage_right_selected = None;
        self.manage_dest_open = true;
        self.manage_refresh(ManageSide::Right);
    }

    fn start_move_check(&self, src: String, dst: String, op_name: String) {
        let ip = self.ip.clone();
        let rt = self.rt.clone();
        let tx = self.tx.clone();
        let req = MoveRequest { src: src.clone(), dst: dst.clone(), op_name: op_name.clone(), dst_exists: false };
        thread::spawn(move || {
            let name = std::path::Path::new(&dst)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent = std::path::Path::new(&dst)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            let exists = if name.is_empty() {
                false
            } else {
                match rt.block_on(async { list_dir(&ip, TRANSFER_PORT, &parent).await }) {
                    Ok(entries) => entries.iter().any(|e| e.name == name),
                    Err(e) => {
                        let _ = tx.send(AppMessage::Log(format!("Move check failed: {}", e)));
                        false
                    }
                }
            };
            let _ = tx.send(AppMessage::MoveCheckResult { req, exists });
        });
    }

    fn note_text(&self, text: &str) -> egui::RichText {
        let color = if self.theme_dark {
            egui::Color32::from_rgb(210, 170, 80)
        } else {
            egui::Color32::from_rgb(130, 85, 0)
        };
        egui::RichText::new(text).color(color).italics()
    }

    fn manage_send_op(&mut self, op: &str, task: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.ip.trim().is_empty() {
            return;
        }
        self.manage_busy = true;
        self.manage_left_status = format!("{}...", op);
        self.log(&format!("{}...", op));
        if op.starts_with("Move") {
            self.is_moving = true;
            self.move_cancellation_token.store(false, Ordering::Relaxed);
        }
        let op_name = op.to_string();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = task();
            let _ = tx.send(AppMessage::ManageOpComplete { op: op_name, result });
        });
    }

    fn start_download(&mut self, label: String, task: impl FnOnce(Sender<AppMessage>, Arc<AtomicBool>) + Send + 'static) {
        if self.ip.trim().is_empty() {
            return;
        }
        self.manage_busy = true;
        self.manage_left_status = format!("{}...", label);
        self.log(&format!("{}...", label));
        self.download_cancellation_token.store(false, Ordering::Relaxed);
        let tx = self.tx.clone();
        let cancel = self.download_cancellation_token.clone();
        task(tx, cancel);
    }

    fn execute_download_request(&mut self, request: DownloadRequest) {
        match request {
            DownloadRequest::File { name, target, save_path } => {
                let ip = self.ip.clone();
                let rt = self.rt.clone();
                let label = format!("Download {}", name);
                self.start_download(label, move |tx, cancel| {
                    thread::spawn(move || {
                        let _ = tx.send(AppMessage::DownloadStart { total: 0, label: target.clone() });
                        let result = rt.block_on(async {
                            download_file_with_progress(&ip, TRANSFER_PORT, &target, &save_path, cancel, |received, total, _| {
                                let _ = tx.send(AppMessage::DownloadProgress {
                                    received,
                                    total,
                                    current_file: None,
                                });
                            }).await
                        });
                        let _ = tx.send(AppMessage::DownloadComplete(result.map_err(|e| e.to_string())));
                    });
                });
            }
            DownloadRequest::Dir { name, target, dest_root, compression } => {
                let ip = self.ip.clone();
                let rt = self.rt.clone();
                let lang_code = self.config.language.clone();
                let label = format!("Download {}", name);
                self.start_download(label, move |tx, cancel| {
                    thread::spawn(move || {
                        let _ = tx.send(AppMessage::DownloadStart { total: 0, label: target.clone() });
                        let result = rt.block_on(async {
                            let lang = Language::from_code(&lang_code);
                            download_dir_with_progress(
                                &ip,
                                TRANSFER_PORT,
                                &target,
                                &dest_root,
                                cancel,
                                compression,
                                |received, total, current_file| {
                                let _ = tx.send(AppMessage::DownloadProgress {
                                    received,
                                    total,
                                    current_file,
                                });
                                },
                                |comp| {
                                    if let Some(comp) = comp {
                                        let msg = if compression == DownloadCompression::Auto {
                                            format!("{} {}", tr(lang, "compression_auto_selected"), comp)
                                        } else {
                                            format!("{} {}", tr(lang, "compression_used"), comp)
                                        };
                                        let _ = tx.send(AppMessage::Log(msg));
                                    }
                                },
                            ).await
                        });
                        let _ = tx.send(AppMessage::DownloadComplete(result.map_err(|e| e.to_string())));
                    });
                });
            }
        }
    }

    fn start_optimize_upload(&mut self) {
        if self.game_path.trim().is_empty() {
            return;
        }
        let path = self.game_path.clone();
        let connections = self.config.connections;
        let tx = self.tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let lang = Language::from_code(&self.config.language);
        self.log(&tr(lang, "optimize_upload_scanning"));
        thread::spawn(move || {
            let opt = optimize_upload_settings(&path, &cancel, connections);
            let _ = tx.send(AppMessage::UploadOptimizeComplete(opt));
        });
    }

    fn start_move_request(&mut self, request: MoveRequest) {
        let ip = self.ip.clone();
        let rt = self.rt.clone();
        let src = request.src.clone();
        let dst = request.dst.clone();
        let op_name = request.op_name.clone();
        self.manage_send_op(&op_name, move || {
            rt.block_on(async {
                move_path(&ip, TRANSFER_PORT, &src, &dst).await
            }).map_err(|e| e.to_string())
        });
    }

    fn start_update_check(&mut self) {
        if self.update_check_running {
            return;
        }
        self.update_check_running = true;
        self.update_status = "Checking for updates...".to_string();
        let include_prerelease = self.config.update_channel == "all";
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        thread::spawn(move || {
            let res = rt.block_on(async { fetch_latest_release(include_prerelease).await });
            let _ = tx.send(AppMessage::UpdateCheckComplete(res.map_err(|e| e.to_string())));
        });
    }

    fn start_self_update(&mut self) {
        let Some(release) = self.update_info.clone() else {
            self.update_download_status = tr(self.language, "update_no_release");
            return;
        };
        let asset_name = match current_asset_name() {
            Ok(name) => name,
            Err(e) => {
                self.update_download_status = format!("{} {}", tr(self.language, "update_asset_missing"), e);
                return;
            }
        };
        let asset = release.assets.iter().find(|a| a.name == asset_name);
        let Some(asset) = asset else {
            self.update_download_status = tr(self.language, "update_asset_missing");
            return;
        };

        self.update_download_status = tr(self.language, "update_downloading");
        self.update_check_running = true;
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        let url = asset.browser_download_url.clone();

        thread::spawn(move || {
            let result = rt.block_on(async move {
                let temp_root = std::env::temp_dir().join(format!("ps5upload_update_{}", chrono::Utc::now().timestamp()));
                std::fs::create_dir_all(&temp_root)?;
                let zip_path = temp_root.join("update.zip");
                download_asset(&url, zip_path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid update path"))?).await?;
                let extract_dir = temp_root.join("extracted");
                extract_zip(&zip_path, &extract_dir)?;
                let pending = build_pending_update(&extract_dir)?;
                Ok::<PendingUpdate, anyhow::Error>(pending)
            });
            let _ = tx.send(AppMessage::SelfUpdateReady(result.map_err(|e| e.to_string())));
        });
    }

    fn apply_self_update(&mut self) {
        let Some(pending) = self.pending_update.clone() else {
            return;
        };
        if let Err(err) = spawn_update_helper(&pending) {
            self.update_download_status = format!("{} {}", tr(self.language, "update_apply_failed"), err);
            return;
        }
        self.update_download_status = tr(self.language, "update_restarting");
        std::process::exit(0);
    }

    fn start_chat(&mut self) {
        if self.chat_tx.is_some() {
            return;
        }
        let key = CHAT_SHARED_KEY_HEX.trim();
        if key.is_empty() {
            self.chat_status = tr(self.language, "chat_disabled_missing_key");
            self.chat_room_id.clear();
            return;
        }
        self.chat_room_id = chat_room_id_for_key(key);
        self.chat_status = tr(self.language, "chat_connecting");
        let tx = self.tx.clone();
        let handle = self.rt.handle().clone();
        let sender = chat::start_chat_worker(tx, key.to_string(), handle);
        self.chat_tx = Some(sender);
    }

    fn send_chat_message(&mut self) {
        self.ensure_chat_display_name();
        let text = self.chat_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let time = chrono::Local::now().format("%H:%M").to_string();
        let sender_label = self.config.chat_display_name.clone();
        let payload = serde_json::json!({
            "name": sender_label,
            "text": text,
        })
        .to_string();
        self.push_chat_message(ChatMessage {
            time,
            sender: self.config.chat_display_name.clone(),
            text: text.clone(),
            local: true,
        });
        if let Some(tx) = &self.chat_tx {
            let _ = tx.send(payload);
            self.chat_sent_count = self.chat_sent_count.saturating_add(1);
            self.log("Chat: message sent.");
        } else {
            self.chat_status = tr(self.language, "chat_disabled_missing_key");
        }
        self.chat_input.clear();
    }

fn format_chat_status(&self, status: ChatStatusEvent) -> String {
        let lang = self.language;
        match status {
            ChatStatusEvent::Connected => tr(lang, "chat_connected"),
            ChatStatusEvent::Disconnected => tr(lang, "chat_disconnected"),
        }
    }

    fn ensure_chat_display_name(&mut self) {
        if !self.config.chat_display_name.trim().is_empty() {
            return;
        }
        let name = generate_chat_display_name(CHAT_SHARED_KEY_HEX);
        self.config.chat_display_name = name;
        let _ = self.config.save();
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
        self.update_check_running = true;
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
    
    fn build_dest_path_with_base(&self, base: &str, include_folder: bool) -> String {
        let base = if base.trim().is_empty() { "/data" } else { base };

        let preset_path = if self.selected_preset == 2 { // custom
             &self.custom_preset_path
        } else {
             PRESETS[self.selected_preset]
        };

        let base_clean = base.trim_end_matches('/');
        let preset_clean = preset_path.trim_matches('/');

        if include_folder {
            let folder = if self.custom_subfolder.is_empty() {
                 "App"
            } else {
                 &self.custom_subfolder
            };
            if preset_clean.is_empty() {
                format!("{}/{}", base_clean, folder)
            } else {
                format!("{}/{}/{}", base_clean, preset_clean, folder)
            }
        } else if preset_clean.is_empty() {
            base_clean.to_string()
        } else {
            format!("{}/{}", base_clean, preset_clean)
        }
    }

    fn get_dest_path(&self) -> String {
        let base = self.selected_storage.as_deref().unwrap_or("/data");
        let is_archive = Self::archive_kind(Path::new(&self.game_path)).is_some();
        let include_folder = !(is_archive && self.pending_archive_trim);
        self.build_dest_path_with_base(base, include_folder)
    }

    fn apply_history_record(&mut self, record: &TransferRecord) -> Result<(), String> {
        if record.source_path.trim().is_empty() || record.dest_path.trim().is_empty() {
            return Err("Missing source or destination path.".to_string());
        }
        if !std::path::Path::new(&record.source_path).exists() {
            return Err("Source folder is not available.".to_string());
        }

        let dest = record.dest_path.trim_end_matches('/');
        let mut base = "/data".to_string();
        let mut best_len = 0usize;
        for loc in &self.storage_locations {
            if dest.starts_with(&loc.path) && loc.path.len() > best_len {
                best_len = loc.path.len();
                base = loc.path.clone();
            }
        }
        if dest.starts_with("/mnt/usb0") && best_len == 0 {
            base = "/mnt/usb0".to_string();
        } else if dest.starts_with("/mnt/ext1") && best_len == 0 {
            base = "/mnt/ext1".to_string();
        }

        let relative = dest.strip_prefix(&base).unwrap_or(dest).trim_start_matches('/');
        let (preset_path, folder_name) = if let Some((parent, name)) = relative.rsplit_once('/') {
            (parent.to_string(), name.to_string())
        } else if relative.is_empty() {
            (String::new(), std::path::Path::new(&record.source_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "App".to_string()))
        } else {
            (String::new(), relative.to_string())
        };

        self.selected_storage = Some(base);
        self.selected_preset = 2;
        self.custom_preset_path = preset_path;
        self.update_game_path(record.source_path.clone());
        self.custom_subfolder = folder_name;
        Ok(())
    }

    fn build_dest_path_for_item(&self, base: &str, item: &QueueItem) -> String {
        let preset_path = if item.preset_index == 2 {
            item.custom_preset_path.trim().to_string()
        } else {
            PRESETS.get(item.preset_index).unwrap_or(&"").to_string()
        };
        let base_clean = base.trim_end_matches('/');
        let preset_clean = preset_path.trim_matches('/');
        if preset_clean.is_empty() {
            format!("{}/{}", base_clean, item.subfolder_name)
        } else {
            format!("{}/{}/{}", base_clean, preset_clean, item.subfolder_name)
        }
    }

    fn update_game_path(&mut self, path: String) {
        self.game_path = path;
        let path_obj = Path::new(&self.game_path);
        if let Some(name) = path_obj.file_name() {
            if path_obj.is_file() && Self::archive_kind(path_obj).is_some() {
                if let Some(stem) = path_obj.file_stem() {
                    self.custom_subfolder = stem.to_string_lossy().to_string();
                } else {
                    self.custom_subfolder = name.to_string_lossy().to_string();
                }
            } else {
                self.custom_subfolder = name.to_string_lossy().to_string();
            }
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

    fn archive_kind(path: &Path) -> Option<&'static str> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match ext.as_str() {
            "rar" => Some("RAR"),
            "zip" => Some("ZIP"),
            "7z" => Some("7Z"),
            _ => None,
        }
    }

    fn prompt_archive_confirm(&mut self, path: &Path, kind: &str) {
        self.pending_archive_path = Some(path.display().to_string());
        self.pending_archive_kind = Some(kind.to_string());
        self.pending_archive_trim = true;
        self.show_archive_confirm_dialog = true;
    }
    
    fn connect(&mut self) {
        // Save config
        self.config.address = self.ip.clone();
        let _ = self.config.save();

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
        self.manage_busy = true;

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
        self.upload_run_id = self.upload_run_id.wrapping_add(1);
        let run_id = self.upload_run_id;

        // Reset state
        self.is_uploading = true;
        self.is_scanning = true;
        self.scanning_files_found = 0;
        self.scanning_total_size = 0;
        self.show_override_dialog = false;
        self.show_resume_dialog = false;
        let lang = Language::from_code(&self.config.language);
        self.status = tr(lang, "status_scanning");
        self.progress_sent = 0;
        self.progress_total = self.calculated_size.unwrap_or(0);
        self.progress_files = 0;
        self.progress_speed_bps = 0.0;
        self.progress_eta_secs = None;
        self.progress_phase.clear();

        // Record for history
        self.upload_start_time = Some(std::time::Instant::now());
        self.upload_source_path = self.game_path.clone();
        self.upload_dest_path = self.get_dest_path();

        let cancel_token = Arc::new(AtomicBool::new(false));
        self.upload_cancellation_token = cancel_token.clone();

        let ip = self.ip.clone();
        let game_path = self.game_path.clone();
        let dest_path = self.forced_dest_path.clone().unwrap_or_else(|| self.get_dest_path());
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        let connections = self.config.connections;
        let use_temp = self.config.use_temp;
        let _archive_trim = self.pending_archive_trim;
        let mut resume_mode = self.config.resume_mode.clone();
        if self.force_full_upload_once {
            resume_mode = "none".to_string();
            self.force_full_upload_once = false;
        }
        let bandwidth_limit_bps = (self.config.bandwidth_limit_mbps * 1024.0 * 1024.0) as u64;
        let auto_tune_connections = self.config.auto_tune_connections;
        let compression_setting = self.config.compression.clone();
        let optimize_upload = self.config.optimize_upload;
        let lang_code = self.config.language.clone();
        let payload_supports_modern = self.payload_supports_modern_compression();
        let required_size = self.calculated_size;
        
        let mut best_root = "/data".to_string();
        let mut best_len = 0;
        let check_dest = dest_path.clone();
        for loc in &self.storage_locations {
             if check_dest.starts_with(&loc.path) && loc.path.len() > best_len {
                 best_len = loc.path.len();
                 best_root = loc.path.clone();
             }
        }
        let storage_root = best_root;

        thread::spawn(move || {
            let tx_log = tx.clone();
            let tx_inner = tx.clone();
            let res = rt.block_on(async move {
                let tx = tx_inner;
                
                if let Some(req) = required_size {
                    let required_safe = req.saturating_add(64 * 1024 * 1024); 
                    match get_space(&ip, TRANSFER_PORT, &storage_root).await {
                        Ok((free_bytes, _total)) => {
                            if free_bytes < required_safe {
                                let _ = tx.send(AppMessage::Log(format!("Insufficient space: {} free, {} required", crate::format_bytes(free_bytes), crate::format_bytes(required_safe))));
                                return Err(anyhow::anyhow!("Not enough free space on target drive"));
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(AppMessage::Log(format!("Space check warning: {}", e)));
                        }
                    }
                }

                if cancel_token.load(Ordering::Relaxed) {
                    return Err(anyhow::anyhow!("Cancelled"));
                }

                let path_low = game_path.to_lowercase();
                let is_rar = path_low.ends_with(".rar");
                let is_zip = path_low.ends_with(".zip");
                let is_7z = path_low.ends_with(".7z");
                let is_archive = is_rar || is_zip || is_7z;
                if is_archive && resume_mode != "none" {
                    resume_mode = "none".to_string();
                    let _ = tx.send(AppMessage::Log("Resume is disabled for archive uploads.".to_string()));
                }
                let effective_path = game_path.clone();

                // RAR files are extracted server-side on PS5
                if is_rar {
                    let _ = tx.send(AppMessage::UploadStart { run_id });
                    let _ = tx.send(AppMessage::Log("Uploading RAR to PS5 for server-side extraction...".to_string()));
                    let _ = tx.send(AppMessage::StatusPhase("Uploading".to_string()));

                    let _rar_size = std::fs::metadata(&game_path)
                        .map(|m| m.len())
                        .unwrap_or(0);

                    let start = std::time::Instant::now();
                    let tx_progress = tx.clone();
                    let progress_callback = move |sent: u64, total: u64| {
                        let elapsed = start.elapsed().as_secs_f64();
                        let _ = tx_progress.send(AppMessage::Progress {
                            run_id,
                            sent,
                            total,
                            files_sent: 0,
                            elapsed_secs: elapsed,
                            current_file: Some("Uploading RAR...".to_string())
                        });
                    };

                    let tx_extract = tx.clone();
                    let result = upload_rar_for_extraction(
                        &ip,
                        TRANSFER_PORT,
                        &game_path,
                        &dest_path,
                        cancel_token.clone(),
                        progress_callback,
                        move |msg| { 
                            let _ = tx_extract.send(AppMessage::Log(msg.clone()));
                            let _ = tx_extract.send(AppMessage::StatusPhase("Extracting".to_string()));
                        }
                    ).await;

                    match result {
                        Ok((files, bytes)) => {
                            let _ = tx.send(AppMessage::Log(format!("PS5 extracted {} files ({} bytes)", files, bytes)));
                            return Ok((files as i32, bytes));
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }

                // ZIP and 7Z archives - client-side handling
                if is_zip || is_7z {
                    let ext = if is_zip { "ZIP" } else { "7Z" };
                    
                    let _ = tx.send(AppMessage::UploadStart { run_id });
                    let _ = tx.send(AppMessage::Log(format!("Starting {} streaming upload...", ext)));

                    let (_count, size) = if is_zip {
                        scan_zip_archive(&game_path)?
                    } else {
                        scan_7z_archive(&game_path)?
                    };

                    let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, use_temp).await?;
                    let mut std_stream = stream.into_std()?;
                    std_stream.set_nonblocking(true)?;
                    let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));

                    let start = std::time::Instant::now();
                    let last_progress_ms = Arc::new(AtomicU64::new(0));
                    let rate_limit = if bandwidth_limit_bps > 0 { Some(bandwidth_limit_bps) } else { None };
                    let mut last_sent = 0u64;

                    if is_zip {
                        send_zip_archive(game_path, std_stream.try_clone()?, cancel_token.clone(), move |sent, files_sent, current_file| {
                            if sent == last_sent { return; }
                            let elapsed = start.elapsed().as_secs_f64();
                            let _ = tx.send(AppMessage::Progress { run_id, sent, total: size, files_sent, elapsed_secs: elapsed, current_file });
                            last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            last_sent = sent;
                        }, move |msg| { let _ = tx_log.send(AppMessage::Log(msg)); }, rate_limit)?;
                    } else {
                        send_7z_archive(game_path, std_stream.try_clone()?, cancel_token.clone(), move |sent, files_sent, current_file| {
                            if sent == last_sent { return; }
                            let elapsed = start.elapsed().as_secs_f64();
                            let _ = tx.send(AppMessage::Progress { run_id, sent, total: size, files_sent, elapsed_secs: elapsed, current_file });
                            last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            last_sent = sent;
                        }, move |msg| { let _ = tx_log.send(AppMessage::Log(msg)); }, rate_limit)?;
                    }

                    let response = read_upload_response(&mut std_stream, &cancel_token)?;
                    return parse_upload_response(&response);
                }

                let mut connection_count_cfg = connections.clamp(1, MAX_PARALLEL_CONNECTIONS);
                let mut optimize_compression: Option<CompressionMode> = None;
                let mut optimize_connections: Option<usize> = None;
                let lang = Language::from_code(&lang_code);
                if !is_archive && optimize_upload {
                    let _ = tx.send(AppMessage::Log(tr(lang, "optimize_upload_scanning")));
                    let opt = optimize_upload_settings(&effective_path, &cancel_token, connection_count_cfg);
                    if opt.compression.is_none() && opt.connections.is_none() {
                        let _ = tx.send(AppMessage::Log(tr(lang, "optimize_upload_no_samples")));
                    } else {
                        optimize_connections = opt.connections;
                        optimize_compression = opt.compression;
                        if let Some(recommended) = optimize_connections {
                            connection_count_cfg = recommended;
                        }
                        let unchanged = tr(lang, "optimize_upload_unchanged");
                        let comp_label = optimize_compression.map(compression_label).unwrap_or(unchanged.as_str());
                        let template = tr(lang, "optimize_upload_result");
                        let msg = template
                            .replacen("{}", comp_label, 1)
                            .replacen("{}", &connection_count_cfg.to_string(), 1);
                        let _ = tx.send(AppMessage::Log(msg));
                    }
                } else if !is_archive && auto_tune_connections {
                    if let Some((sample_count, sample_bytes)) = sample_workload(&effective_path, &cancel_token) {
                        let recommended = recommend_connections(connection_count_cfg, sample_count, sample_bytes);
                        if recommended != connection_count_cfg {
                            let _ = tx.send(AppMessage::Log(format!(
                                "Auto-tune: detected small files, using {} connection{} for better throughput.",
                                recommended,
                                if recommended == 1 { "" } else { "s" }
                            )));
                            connection_count_cfg = recommended;
                        }
                    }
                }
                let can_stream = resume_mode == "none";

                if can_stream {
                     let _ = tx.send(AppMessage::UploadStart { run_id });
                     let _ = tx.send(AppMessage::Log(format!("Starting streaming upload ({} connections)...", connection_count_cfg)));
                     
                     let tx_scan = tx.clone();
                     let shared_total = Arc::new(AtomicU64::new(0));
                     let shared_total_scan = shared_total.clone();
                     let rx = stream_files_with_progress(
                        effective_path.clone(),
                        cancel_token.clone(),
                        move |files_found, total_size| {
                            shared_total_scan.store(total_size, Ordering::Relaxed);
                            let _ = tx_scan.send(AppMessage::Scanning { run_id, files_found, total_size });
                        }
                     );

                     let start = std::time::Instant::now();
                     let last_progress_ms = Arc::new(AtomicU64::new(0));
                     let mut compression = match compression_setting.as_str() {
                         "lz4" => CompressionMode::Lz4,
                         "zstd" => CompressionMode::Zstd,
                         "lzma" => CompressionMode::Lzma,
                         "auto" => {
                             let _ = tx.send(AppMessage::Log(tr(lang, "compression_auto_scanning")));
                             if let Some(sample) = sample_bytes_from_path(&effective_path, &cancel_token) {
                                 let mode = choose_best_compression(&sample);
                                 let template = tr(lang, "compression_auto_result");
                                 let msg = template.replacen("{}", compression_label(mode), 1);
                                 let _ = tx.send(AppMessage::Log(msg));
                                 mode
                             } else {
                                 CompressionMode::None
                             }
                         }
                         _ => CompressionMode::None,
                     };
                     if let Some(override_mode) = optimize_compression {
                         compression = override_mode;
                     }
                     if matches!(compression, CompressionMode::Zstd | CompressionMode::Lzma)
                         && !payload_supports_modern
                     {
                         let _ = tx.send(AppMessage::Log(
                             "Payload does not support Zstd/LZMA yet; falling back to LZ4.".to_string()
                         ));
                         compression = CompressionMode::Lz4;
                     }
                     let rate_limit = if bandwidth_limit_bps > 0 { 
                         let per_conn = (bandwidth_limit_bps / connection_count_cfg as u64).max(1);
                         Some(per_conn)
                     } else { None };

                     if connection_count_cfg == 1 {
                         let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, use_temp).await?;
                         let mut std_stream = stream.into_std()?;
                         std_stream.set_nonblocking(true)?;
                         let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));
                         
                         let mut last_sent = 0u64;
                         
                         send_files_v2_for_list(
                            rx,
                            std_stream.try_clone()?,
                            SendFilesConfig {
                                cancel: cancel_token.clone(),
                                progress: move |sent, files_sent, current_file| {
                                    if sent == last_sent { return; }
                                    let elapsed = start.elapsed().as_secs_f64();
                                    let current_total = shared_total.load(Ordering::Relaxed);
                                    let display_total = current_total.max(sent);
                                    let _ = tx.send(AppMessage::Progress {
                                        run_id,
                                        sent,
                                        total: display_total,
                                        files_sent,
                                        elapsed_secs: elapsed,
                                        current_file,
                                    });
                                    last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                                    last_sent = sent;
                                },
                                log: move |msg| { let _ = tx_log.send(AppMessage::Log(msg)); },
                                worker_id: 0,
                                allowed_connections: None,
                                compression,
                                rate_limit_bps: rate_limit,
                            }
                         )?;
                         
                         let response = read_upload_response(&mut std_stream, &cancel_token)?;
                         let (files_ack, bytes_ack) = parse_upload_response(&response)?;
                         return Ok((files_ack, bytes_ack));
                     } else {
                         let shared_rx = Arc::new(Mutex::new(rx));
                         let total_sent = Arc::new(AtomicU64::new(0));
                         let total_files = Arc::new(AtomicUsize::new(0));
                         let allowed_connections = Arc::new(AtomicUsize::new(connection_count_cfg));
                         
                         let mut handles = Vec::new();
                         
                         let max_connections = connection_count_cfg;
                         let allowed_monitor = allowed_connections.clone();
                         let last_progress_monitor = last_progress_ms.clone();
                         let cancel_monitor = cancel_token.clone();
                         let start_monitor = start;
                         thread::spawn(move || {
                            let mut stable_good = 0u8;
                            loop {
                                if cancel_monitor.load(Ordering::Relaxed) { break; }
                                let elapsed_ms = start_monitor.elapsed().as_millis() as u64;
                                let last_ms = last_progress_monitor.load(Ordering::Relaxed);
                                if last_ms == 0 { thread::sleep(std::time::Duration::from_millis(500)); continue; }
                                let since = elapsed_ms.saturating_sub(last_ms);
                                if since > 2000 {
                                    let current = allowed_monitor.load(Ordering::Relaxed);
                                    if current > 1 { allowed_monitor.store(current - 1, Ordering::Relaxed); }
                                    stable_good = 0;
                                } else if since < 500 {
                                    stable_good = stable_good.saturating_add(1);
                                    if stable_good >= 6 {
                                        let current = allowed_monitor.load(Ordering::Relaxed);
                                        if current < max_connections { allowed_monitor.store(current + 1, Ordering::Relaxed); }
                                        stable_good = 0;
                                    }
                                } else { stable_good = 0; }
                                thread::sleep(std::time::Duration::from_millis(500));
                            }
                         });

                         let mut streams = Vec::new();
                         for _ in 0..connection_count_cfg {
                             let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, false).await?;
                             let std_stream = stream.into_std()?;
                             std_stream.set_nonblocking(true)?;
                             streams.push(std_stream);
                         }
                         let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));

                         for (worker_id, std_stream) in streams.into_iter().enumerate() {
                             let iterator = SharedReceiverIterator::new(shared_rx.clone());
                             let cancel = cancel_token.clone();
                             let tx = tx.clone();
                             let tx_log = tx_log.clone();
                             let total_sent = total_sent.clone();
                             let total_files = total_files.clone();
                             let shared_total = shared_total.clone();
                             let allowed = allowed_connections.clone();
                             let last_progress = last_progress_ms.clone();
                             
                             handles.push(thread::spawn(move || -> anyhow::Result<()> {
                                 let mut last_sent = 0u64;
                                 let mut last_files = 0i32;
                                 
                                 send_files_v2_for_list(
                                     iterator,
                                     std_stream,
                                     SendFilesConfig {
                                         cancel,
                                         progress: move |sent, files_sent, _| {
                                         let delta_bytes = sent.saturating_sub(last_sent);
                                         let delta_files = if files_sent >= last_files { files_sent - last_files } else { 0 };
                                         if delta_bytes == 0 && delta_files == 0 { return; }
                                         last_sent = sent;
                                         last_files = files_sent;
                                         
                                         let new_total = total_sent.fetch_add(delta_bytes, Ordering::Relaxed) + delta_bytes;
                                         let new_files = total_files.fetch_add(delta_files as usize, Ordering::Relaxed) + delta_files as usize;
                                         let elapsed = start.elapsed().as_secs_f64();
                                         let current_total_scan = shared_total.load(Ordering::Relaxed);
                                         let display_total = current_total_scan.max(new_total);
                                         
                                         let _ = tx.send(AppMessage::Progress {
                                             run_id,
                                             sent: new_total,
                                             total: display_total,
                                             files_sent: new_files as i32,
                                             elapsed_secs: elapsed,
                                             current_file: None,
                                         });
                                         last_progress.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                                     },
                                         log: move |msg| { let _ = tx_log.send(AppMessage::Log(msg)); },
                                         worker_id,
                                         allowed_connections: Some(allowed),
                                         compression,
                                         rate_limit_bps: rate_limit,
                                     }
                                 )
                             }));
                         }
                         
                         let mut first_err = None;
                         for h in handles {
                             if let Ok(Err(e)) = h.join() {
                                 if first_err.is_none() {
                                     first_err = Some(e);
                                 }
                             }
                         }
                         if let Some(e) = first_err { return Err(e); }
                         
                         // We don't read response for multi-connection here as threads finished.
                         // But threads return Ok(()).
                         // We rely on local counting.
                         return Ok((total_files.load(Ordering::Relaxed) as i32, total_sent.load(Ordering::Relaxed)));
                     }
                }

                // Scan files with progress reporting
                let tx_scan = tx.clone();
                let cancel_scan = cancel_token.clone();
                let (mut files, was_cancelled) = collect_files_with_progress(
                    &effective_path,
                    cancel_scan,
                    move |files_found, total_size| {
                        let _ = tx_scan.send(AppMessage::Scanning { run_id, files_found, total_size });
                    }
                );

                if was_cancelled {
                    return Err(anyhow::anyhow!("Cancelled"));
                }

                if files.is_empty() {
                    return Err(anyhow::anyhow!("No files found to upload"));
                }

                if resume_mode != "none" {
                    let mode_label = match resume_mode.as_str() {
                        "size" => "size only (fast)",
                        "size_mtime" => "size + time (medium)",
                        "sha256" => "SHA256 (slow)",
                        _ => "off",
                    };
                    let _ = tx.send(AppMessage::Log(format!("Resume mode: {}", mode_label)));
                    let _ = tx.send(AppMessage::Log("Resume: scanning remote files...".to_string()));
                    let dest_exists = check_dir(&ip, TRANSFER_PORT, &dest_path).await.unwrap_or(false);
                    if !dest_exists {
                        let _ = tx.send(AppMessage::Log("Resume: destination not found, uploading everything.".to_string()));
                    }
                    let remote = if dest_exists {
                        list_dir_recursive(&ip, TRANSFER_PORT, &dest_path).await
                            .map_err(|e| anyhow::anyhow!("Resume failed: {}", e))?
                    } else {
                        std::collections::HashMap::new()
                    };
                    let mut kept = Vec::with_capacity(files.len());
                    let mut skipped_files = 0u64;
                    let mut skipped_bytes = 0u64;

                    for file in files.into_iter() {
                        let Some(remote_entry) = remote.get(&file.rel_path) else {
                            kept.push(file);
                            continue;
                        };
                        let mut skip = false;
                        match resume_mode.as_str() {
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
                                    let local_hash = sha256_file(&file.abs_path)?;
                                    match hash_file(&ip, TRANSFER_PORT, &format!("{}/{}", dest_path.trim_end_matches('/'), file.rel_path)).await {
                                        Ok(remote_hash) => {
                                            skip = local_hash.eq_ignore_ascii_case(&remote_hash);
                                        }
                                        Err(e) => {
                                            let _ = tx.send(AppMessage::Log(format!("Resume hash check failed for {}: {}", file.rel_path, e)));
                                        }
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
                        let _ = tx.send(AppMessage::Log(format!(
                            "Resume: skipped {} file{} ({})",
                            skipped_files,
                            if skipped_files == 1 { "" } else { "s" },
                            format_bytes(skipped_bytes)
                        )));
                    }
                    if files.is_empty() {
                        let _ = tx.send(AppMessage::Log("Resume: nothing left to upload.".to_string()));
                        return Ok((0, 0));
                    }
                }

                let total_size: u64 = files.iter().map(|f| f.size).sum();
                let mut connection_count = connections.clamp(1, MAX_PARALLEL_CONNECTIONS);
                if let Some(recommended) = optimize_connections {
                    connection_count = recommended;
                } else if !is_archive && auto_tune_connections {
                    let recommended = recommend_connections(connection_count, files.len(), total_size);
                    if recommended != connection_count {
                        let _ = tx.send(AppMessage::Log(format!(
                            "Auto-tune: detected small files, using {} connection{} for better throughput.",
                            recommended,
                            if recommended == 1 { "" } else { "s" }
                        )));
                        connection_count = recommended;
                    }
                }
                if files.len() < connection_count {
                    connection_count = files.len().max(1);
                }
                let mut effective_use_temp = use_temp;
                if connection_count > 1 && effective_use_temp {
                    effective_use_temp = false;
                    let _ = tx.send(AppMessage::Log(
                        "Temp staging disabled for multi-connection uploads to avoid corruption.".to_string()
                    ));
                }

                let _ = tx.send(AppMessage::Log(format!(
                    "Starting transfer: {:.2} GB using {} connection{}",
                    total_size as f64 / 1_073_741_824.0,
                    connection_count,
                    if connection_count == 1 { "" } else { "s" }
                )));
                let _ = tx.send(AppMessage::UploadStart { run_id });

                let start = std::time::Instant::now();
                let last_progress_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let mut compression = match compression_setting.as_str() {
                    "lz4" => CompressionMode::Lz4,
                    "zstd" => CompressionMode::Zstd,
                    "lzma" => CompressionMode::Lzma,
                    "auto" => {
                        let _ = tx.send(AppMessage::Log(tr(lang, "compression_auto_scanning")));
                        if let Some(sample) = sample_bytes_from_files(&files, &cancel_token) {
                            let mode = choose_best_compression(&sample);
                            let template = tr(lang, "compression_auto_result");
                            let msg = template.replacen("{}", compression_label(mode), 1);
                            let _ = tx.send(AppMessage::Log(msg));
                            mode
                        } else {
                            CompressionMode::None
                        }
                    }
                    _ => CompressionMode::None,
                };
                if let Some(override_mode) = optimize_compression {
                    compression = override_mode;
                }
                if matches!(compression, CompressionMode::Zstd | CompressionMode::Lzma)
                    && !payload_supports_modern
                {
                    let _ = tx.send(AppMessage::Log(
                        "Payload does not support Zstd/LZMA yet; falling back to LZ4.".to_string()
                    ));
                    compression = CompressionMode::Lz4;
                }
                let rate_limit = if bandwidth_limit_bps > 0 {
                    let per_conn = (bandwidth_limit_bps / connection_count as u64).max(1);
                    Some(per_conn)
                } else {
                    None
                };

                if connection_count == 1 {
                    let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, effective_use_temp).await?;
                    let mut std_stream = stream.into_std()?;
                    std_stream.set_nonblocking(true)?;
                    let _ = tx.send(AppMessage::PayloadLog("Server READY".to_string()));

                    let mut last_sent = 0u64;
                    send_files_v2_for_list(
                        files,
                        std_stream.try_clone()?,
                        SendFilesConfig {
                            cancel: cancel_token.clone(),
                            progress: |sent, files_sent, current_file| {
                                if sent == last_sent { return; }
                                let elapsed = start.elapsed().as_secs_f64();
                                let _ = tx.send(AppMessage::Progress {
                                    run_id,
                                    sent,
                                    total: total_size,
                                    files_sent,
                                    elapsed_secs: elapsed,
                                    current_file,
                                });
                                last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                                last_sent = sent;
                            },
                            log: move |msg| {
                                let _ = tx_log.send(AppMessage::Log(msg));
                            },
                            worker_id: 0,
                            allowed_connections: None,
                            compression,
                            rate_limit_bps: rate_limit,
                        },
                    )?;

                    let response = read_upload_response(&mut std_stream, &cancel_token)?;
                    return parse_upload_response(&response);
                }

                let buckets = partition_files_by_size(files, connection_count);
                let total_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let total_files = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let allowed_connections = Arc::new(std::sync::atomic::AtomicUsize::new(connection_count));
                let mut handles = Vec::new();

                let mut workers = Vec::new();
                for bucket in buckets.into_iter().filter(|b| !b.is_empty()) {
                    let stream = upload_v2_init(&ip, TRANSFER_PORT, &dest_path, effective_use_temp).await?;
                    let std_stream = stream.into_std()?;
                    std_stream.set_nonblocking(true)?;
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
                    let allowed_connections = allowed_connections.clone();
                    let last_progress_ms = last_progress_ms.clone();

                    handles.push(thread::spawn(move || -> anyhow::Result<()> {
                        let mut last_sent = 0u64;
                        let mut last_files = 0i32;
                        send_files_v2_for_list(
                            bucket,
                            std_stream.try_clone()?,
                            SendFilesConfig {
                                cancel: cancel.clone(),
                                progress: |sent, files_sent, current_file| {
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
                                    run_id,
                                    sent: new_total,
                                    total: total_size,
                                    files_sent: new_files as i32,
                                    elapsed_secs: elapsed,
                                    current_file,
                                });
                                last_progress_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                                },
                                log: move |msg| {
                                    let _ = tx_log.send(AppMessage::Log(msg));
                                },
                                worker_id,
                                allowed_connections: Some(allowed_connections),
                                compression,
                                rate_limit_bps: rate_limit,
                            },
                        )?;

                        let response = read_upload_response(&mut std_stream, &cancel)?;
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
            
            let _ = tx.send(AppMessage::UploadComplete { run_id, result: res.map_err(|e| e.to_string()) });
        });
    }

    fn send_payload(&mut self) {
        if self.ip.trim().is_empty() {
            self.payload_log("Enter a PS5 address first.");
            return;
        }
        if self.payload_path.trim().is_empty() {
            self.payload_log("Select a payload (.elf/.bin) file first.");
            return;
        }
        if !payload_path_is_elf(&self.payload_path) {
            self.payload_log("Payload must be a .elf or .bin file.");
            return;
        }

        self.is_sending_payload = true;
        let lang = Language::from_code(&self.config.language);
        self.status = tr(lang, "status_sending_payload");
        self.payload_log(&format!("Sending payload to {}:{}...", self.ip, PAYLOAD_PORT));
        if !self.payload_path.trim().is_empty() {
            self.payload_log(&format!("Payload path: {}", self.payload_path));
        }

        let ip = self.ip.clone();
        let path = self.payload_path.clone();
        let tx = self.tx.clone();

        thread::spawn(move || {
            let result = send_payload_file(&ip, &path, &tx);
            let _ = tx.send(AppMessage::PayloadSendComplete(result));
        });
    }

    fn start_payload_download_and_send(&mut self, fetch: PayloadFetch) {
        if self.ip.trim().is_empty() {
            self.payload_log("Enter a PS5 address first.");
            return;
        }
        if self.is_sending_payload {
            self.payload_log("Payload transfer already in progress.");
            return;
        }

        let (log_label, tmp_name, tag) = match fetch {
            PayloadFetch::Current => {
                let tag = format!("v{}", app_version_trimmed());
                (format!("Downloading payload {}...", tag), "ps5upload_current.elf".to_string(), Some(tag))
            }
            PayloadFetch::Latest => ("Downloading latest payload...".to_string(), "ps5upload_latest.elf".to_string(), None),
        };

        let ip = self.ip.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        self.is_sending_payload = true;
        let lang = Language::from_code(&self.config.language);
        self.status = tr(lang, "status_downloading_payload");
        self.payload_log(&log_label);

        thread::spawn(move || {
            let result = rt.block_on(async {
                let release = if let Some(tag) = tag {
                    match fetch_release_by_tag(&tag).await {
                        Ok(release) => release,
                        Err(_) => {
                            let _ = tx.send(AppMessage::PayloadLog(format!(
                                "Tag {} not found, falling back to latest release.",
                                tag
                            )));
                            fetch_latest_release(false).await?
                        }
                    }
                } else {
                    fetch_latest_release(false).await?
                };
                let asset = release.assets.iter().find(|a| a.name == "ps5upload.elf")
                    .ok_or_else(|| anyhow::anyhow!("Payload asset not found"))?;
                let tmp_path = std::env::temp_dir().join(tmp_name);
                download_asset(&asset.browser_download_url, &tmp_path.display().to_string()).await?;
                Ok::<_, anyhow::Error>(tmp_path)
            });
            match result {
                Ok(path) => {
                    let path_str = path.display().to_string();
                    let _ = tx.send(AppMessage::PayloadLog(format!("Payload downloaded: {}", path_str)));
                    let result = send_payload_file(&ip, &path_str, &tx);
                    let _ = tx.send(AppMessage::PayloadSendComplete(result));
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::PayloadSendComplete(Err(e.to_string())));
                }
            }
        });
    }

    fn payload_supports_modern_compression(&self) -> bool {
        let Some(version) = &self.payload_version else {
            return false;
        };
        let current_norm = normalize_version("1.1.6");
        let running_norm = normalize_version(version);
        match (semver::Version::parse(&running_norm), semver::Version::parse(&current_norm)) {
            (Ok(running), Ok(required)) => running >= required,
            _ => false,
        }
    }

    fn check_payload_version(&mut self) {
        if self.ip.trim().is_empty() {
            return;
        }
        let ip = self.ip.clone();
        let tx = self.tx.clone();
        let rt = self.rt.clone();
        self.last_payload_check = Some(std::time::Instant::now());
        thread::spawn(move || {
            let res = rt.block_on(async { get_payload_version(&ip, TRANSFER_PORT).await });
            let _ = tx.send(AppMessage::PayloadVersion(res.map_err(|e| e.to_string())));
        });
    }
}

fn generate_chat_display_name(key_hex: &str) -> String {
    let mut rng = rand::thread_rng();
    let rand_part: u32 = rng.gen();
    let mut tag = String::new();
    let trimmed = key_hex.trim();
    if !trimmed.is_empty() {
        let digest = sha2::Sha256::digest(trimmed.as_bytes());
        tag = hex::encode(digest);
    }
    let tag_short = if tag.is_empty() { "chat".to_string() } else { tag.chars().take(4).collect() };
    let time = chrono::Utc::now().timestamp() as u64;
    let suffix = format!("{:08x}{:08x}", time as u32, rand_part);
    format!("Player-{}-{}", tag_short, &suffix[..6])
}

fn payload_path_is_elf(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let e = ext.to_lowercase();
            e == "elf" || e == "bin"
        })
        .unwrap_or(false)
}

fn send_payload_file(ip: &str, path: &str, tx: &Sender<AppMessage>) -> Result<u64, String> {
    use std::fs::File;
    use std::net::TcpStream;
    use std::time::Duration;

    if !payload_path_is_elf(path) {
        return Err("Payload must be a .elf or .bin file.".to_string());
    }

    let mut file = File::open(path)
        .map_err(|e| format!("Failed to open payload: {}", e))?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file_len > 0 {
        let size_msg = crate::format_bytes(file_len);
        let _ = tx.send(AppMessage::PayloadLog(format!("Payload size: {}", size_msg)));
    }
    let mut stream = TcpStream::connect((ip, PAYLOAD_PORT))
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

impl eframe::App for Ps5UploadApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.is_uploading || self.is_downloading || self.is_connecting || self.manage_busy || self.is_sending_payload || self.calculating_size || self.update_check_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        let connected = self.is_connected || self.is_uploading;
        let lang = self.language;

        if self.config.auto_check_payload && connected && !self.is_sending_payload {
            let now = std::time::Instant::now();
            let should_check = self.last_payload_check
                .map(|last| now.duration_since(last).as_secs() >= 30)
                .unwrap_or(true);
            if should_check {
                self.check_payload_version();
            }
        }
        if self.logo_texture.is_none() {
            if let Some(image) = load_logo_image() {
                self.logo_texture = Some(ctx.load_texture(
                    "ps5upload_logo",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMessage::Log(s) => self.log(&s),
                AppMessage::PayloadLog(s) => self.payload_log(&s),
                AppMessage::StatusPhase(phase) => self.progress_phase = phase,
                AppMessage::ChatMessage(msg) => {
                    self.chat_received_count = self.chat_received_count.saturating_add(1);
                    self.log("Chat: message received.");
                    self.push_chat_message(msg);
                }
                AppMessage::ChatAck { ok, reason } => {
                    if ok {
                        self.chat_ack_count = self.chat_ack_count.saturating_add(1);
                    } else {
                        self.chat_reject_count = self.chat_reject_count.saturating_add(1);
                        if let Some(reason) = reason {
                            let trimmed = reason.to_lowercase();
                            if trimmed.contains("auth") || trimmed.contains("sign") || trimmed.contains("signup") {
                                // Keep noisy relay policy messages out of the UI.
                            }
                        }
                    }
                }
                AppMessage::ChatStatus(status) => self.chat_status = self.format_chat_status(status),
                AppMessage::StorageList(res) => {
                    self.is_connecting = false;
                    match res {
                        Ok(locs) => {
                            self.storage_locations = locs.into_iter().filter(|l| l.free_gb > 0.0).collect();
                            if let Some(first) = self.storage_locations.first() {
                                self.selected_storage = Some(first.path.clone());
                                if self.manage_left_path.trim().is_empty() || self.manage_left_path == "/data" {
                                    self.manage_left_path = first.path.clone();
                                }
                                if self.manage_right_path.trim().is_empty() || self.manage_right_path == "/data" {
                                    self.manage_right_path = first.path.clone();
                                }
                            }
                            self.status = "Connected".to_string();
                            self.is_connected = true;
                            self.log("Connected to PS5");
                            self.payload_log("Connected and Storage scanned.");
                            self.check_payload_version();
                            if self.main_tab == 1 {
                                self.manage_refresh(ManageSide::Left);
                            }
                        }
                        Err(e) => {
                            self.log(&format!("Error: {}", e));
                            self.status = "Connection Failed".to_string();
                            self.is_connected = false;
                        }
                    }
                }
                AppMessage::ManageList { side, result: res } => {
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
                            match side {
                                ManageSide::Left => {
                                    self.manage_left_entries = entries;
                                    self.manage_left_selected = None;
                                    self.manage_left_status = format!("{} item{}", count, if count == 1 { "" } else { "s" });
                                }
                                ManageSide::Right => {
                                    self.manage_right_entries = entries;
                                    self.manage_right_selected = None;
                                    self.manage_right_status = format!("{} item{}", count, if count == 1 { "" } else { "s" });
                                }
                            }
                        }
                        Err(e) => {
                            match side {
                                ManageSide::Left => self.manage_left_status = format!("List failed: {}", e),
                                ManageSide::Right => self.manage_right_status = format!("List failed: {}", e),
                            }
                            self.log(&format!("List failed: {}", e));
                        }
                    }
                }
                AppMessage::ManageOpComplete { op, result } => {
                    self.manage_busy = false;
                    if op.starts_with("Move") {
                        self.is_moving = false;
                    }
                    match result {
                        Ok(()) => {
                            if op.starts_with("Move") && self.move_cancellation_token.load(Ordering::Relaxed) {
                                self.manage_left_status = "Move cancelled".to_string();
                                self.log("Move cancelled (operation may complete)");
                            } else {
                                self.manage_left_status = format!("{} OK", op);
                                self.log(&format!("{} OK", op));
                            }
                            self.manage_refresh(ManageSide::Left);
                        }
                        Err(e) => {
                            self.manage_left_status = format!("{} failed: {}", op, e);
                            self.log(&format!("{} failed: {}", op, e));
                        }
                    }
                }
                AppMessage::MoveCheckResult { mut req, exists } => {
                    req.dst_exists = exists;
                    self.pending_move_request = Some(req);
                    self.show_move_overwrite_dialog = true;
                    self.status = "Confirm Move".to_string();
                }
                AppMessage::DownloadStart { total, label } => {
                    self.is_downloading = true;
                    self.status = tr(lang, "status_downloading");
                    self.download_progress_sent = 0;
                    self.download_progress_total = total;
                    self.download_speed_bps = 0.0;
                    self.download_eta_secs = None;
                    self.download_current_file = label.clone();
                    self.download_start_time = Some(std::time::Instant::now());
                    self.log(&format!("Download started: {}", label));
                }
                AppMessage::DownloadProgress { received, total, current_file } => {
                    self.download_progress_sent = received;
                    self.download_progress_total = total.max(received);
                    if let Some(name) = current_file {
                        if !name.is_empty() {
                            self.download_current_file = name;
                        }
                    }
                    if let Some(start) = self.download_start_time {
                        let elapsed = start.elapsed().as_secs_f64();
                        if elapsed > 0.0 {
                            self.download_speed_bps = received as f64 / elapsed;
                            if self.download_progress_total > received && self.download_speed_bps > 0.0 {
                                let remaining = (self.download_progress_total - received) as f64;
                                self.download_eta_secs = Some(remaining / self.download_speed_bps);
                            }
                        }
                    }
                }
                AppMessage::DownloadComplete(res) => {
                    self.manage_busy = false;
                    self.is_downloading = false;
                    self.download_start_time = None;
                    match res {
                        Ok(bytes) => {
                            self.log(&format!("✓ Download complete: {}", format_bytes(bytes)));
                            self.status = "Download Complete!".to_string();
                        }
                        Err(e) => {
                            self.log(&format!("✗ Download failed: {}", e));
                            self.status = "Download Failed".to_string();
                        }
                    }
                }
                AppMessage::UploadOptimizeComplete(opt) => {
                    let lang = Language::from_code(&self.config.language);
                    if opt.compression.is_none() && opt.connections.is_none() {
                        self.log(&tr(lang, "optimize_upload_no_samples"));
                        return;
                    }
                    if let Some(mode) = opt.compression {
                        self.config.compression = compression_mode_to_setting(mode).to_string();
                    }
                    if let Some(connections) = opt.connections {
                        self.config.connections = connections;
                    }
                    let _ = self.config.save();
                    let unchanged = tr(lang, "optimize_upload_unchanged");
                    let comp_label = opt.compression.map(compression_label).unwrap_or(unchanged.as_str());
                    let conn_label = opt.connections.map(|v| v.to_string()).unwrap_or_else(|| unchanged.clone());
                    let template = tr(lang, "optimize_upload_result");
                    let msg = template.replacen("{}", comp_label, 1).replacen("{}", &conn_label, 1);
                    self.log(&msg);
                }
                AppMessage::UpdateCheckComplete(res) => {
                    self.update_check_running = false;
                    match res {
                        Ok(release) => {
                            let current = app_version_trimmed();
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
                    self.update_check_running = false;
                    match result {
                        Ok(path) => {
                            self.update_download_status = format!("Downloaded {} to {}", kind, path);
                        }
                        Err(e) => {
                            self.update_download_status = format!("Download {} failed: {}", kind, e);
                        }
                    }
                }
                AppMessage::SelfUpdateReady(result) => {
                    self.update_check_running = false;
                    match result {
                        Ok(pending) => {
                            self.pending_update = Some(pending);
                            self.update_download_status = tr(lang, "update_ready");
                        }
                        Err(e) => {
                            self.update_download_status = format!("{} {}", tr(lang, "update_failed"), e);
                        }
                    }
                }
                AppMessage::PayloadSendComplete(res) => {
                    self.is_sending_payload = false;
                    match res {
                        Ok(bytes) => {
                            self.payload_log(&format!("Payload sent ({}).", format_bytes(bytes)));
                            self.status = "Payload sent".to_string();
                            self.check_payload_version();
                        }
                        Err(e) => {
                            self.payload_log(&format!("Payload failed: {}", e));
                            self.status = "Payload failed".to_string();
                        }
                    }
                }
                AppMessage::PayloadVersion(res) => {
                    match res {
                        Ok(version) => {
                            self.payload_status = format!("Running (v{})", version);
                            self.payload_version = Some(version.clone());
                            let current_version = normalize_version(app_version_trimmed());
                            let running_version = normalize_version(&version);
                            if self.config.auto_check_payload
                                && running_version != current_version
                            {
                                self.payload_log(&format!(
                                    "Payload version mismatch (v{}). Auto-loading current payload...",
                                    version
                                ));
                                self.start_payload_download_and_send(PayloadFetch::Current);
                            }
                        }
                        Err(e) => {
                            self.payload_status = format!("Not detected ({})", e);
                            self.payload_version = None;
                            if self.config.auto_check_payload {
                                self.payload_log("Payload not detected. Auto-loading current payload...");
                                self.start_payload_download_and_send(PayloadFetch::Current);
                            }
                        }
                    }
                }
                AppMessage::CheckExistsResult(exists) => {
                    self.manage_busy = false;
                    self.status = "Connected".to_string();
                    if exists {
                        let path_obj = std::path::Path::new(&self.game_path);
                        if Self::archive_kind(path_obj).is_some() {
                            self.show_archive_overwrite_dialog = true;
                            self.archive_overwrite_confirmed = true;
                        } else {
                            self.show_override_dialog = true;
                        }
                    } else {
                        self.start_upload();
                    }
                }
                AppMessage::SizeCalculated(size) => {
                    self.calculating_size = false;
                    self.calculated_size = Some(size);
                }
                AppMessage::Scanning { run_id, files_found, total_size } => {
                    if run_id != self.upload_run_id {
                        continue;
                    }
                    self.scanning_files_found = files_found;
                    self.scanning_total_size = total_size;
                    if self.is_scanning {
                        let template = tr(lang, "status_scanning_detail");
                        let msg = template
                            .replacen("{}", &files_found.to_string(), 1)
                            .replacen("{}", &format_bytes(total_size), 1);
                        self.status = msg;
                        self.progress_total = total_size;
                        self.progress_phase = "Scanning".to_string();
                    }
                    if self.is_uploading && total_size > self.progress_total {
                        self.progress_total = total_size;
                    }
                }
                AppMessage::UploadStart { run_id } => {
                    if run_id != self.upload_run_id {
                        continue;
                    }
                    self.is_scanning = false;
                    self.status = tr(lang, "status_uploading");
                    self.progress_phase = "Sending".to_string();
                }
                AppMessage::Progress { run_id, sent, total, files_sent, elapsed_secs, current_file } => {
                    if run_id != self.upload_run_id {
                        continue;
                    }
                    self.progress_sent = sent;
                    self.progress_total = total;
                    self.progress_files = files_sent;
                    if sent == 0 {
                        self.progress_phase = "Packing".to_string();
                    } else {
                        self.progress_phase = "Sending".to_string();
                    }
                    if elapsed_secs > 0.0 { self.progress_speed_bps = sent as f64 / elapsed_secs; }
                    if total > sent && self.progress_speed_bps > 0.0 {
                        let remaining = (total - sent) as f64;
                        self.progress_eta_secs = Some(remaining / self.progress_speed_bps);
                    }
                    if let Some(name) = current_file {
                        self.progress_current_file = name;
                    }
                }
                AppMessage::UploadComplete { run_id, result: res } => {
                    if run_id != self.upload_run_id {
                        continue;
                    }
                    self.is_uploading = false;
                    self.is_scanning = false;
                    self.progress_phase.clear();
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
                                via_queue: self.current_queue_item_id.is_some(),
                            };
                            add_record(&mut self.history_data, record);

                            if self.config.chmod_after_upload {
                                let ip = self.ip.clone();
                                let path = self.upload_dest_path.clone();
                                let tx = self.tx.clone();
                                let rt = self.rt.clone();
                                self.log(&format!("Applying chmod 777 recursively to {}...", path));
                                thread::spawn(move || {
                                    let result = rt.block_on(async { chmod_777(&ip, TRANSFER_PORT, &path).await });
                                    match result {
                                        Ok(_) => {
                                            let _ = tx.send(AppMessage::Log(format!("chmod 777 complete for {}", path)));
                                        }
                                        Err(e) => {
                                            let _ = tx.send(AppMessage::Log(format!("chmod 777 failed for {}: {}", path, e)));
                                        }
                                    }
                                });
                            }

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
                                via_queue: self.current_queue_item_id.is_some(),
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
                    self.log_peak_rss("upload");
                    self.upload_start_time = None;
                }
            }
        }

        if self.show_archive_overwrite_dialog {
            egui::Window::new(tr(lang, "confirm_overwrite"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Archive Extraction Warning\n\nThe destination folder already exists:\n{}\n\nExtracting this archive will merge its contents with the existing folder. Any files with the same name will be overwritten.", self.get_dest_path()));
                    ui.add_space(10.0);
                    
                    ui.checkbox(&mut self.archive_overwrite_confirmed, "Allow overwriting of existing files");
                    ui.label(egui::RichText::new("If unchecked, the upload will be cancelled to prevent data loss.").weak());
                    
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "continue")).clicked() {
                            if self.archive_overwrite_confirmed {
                                self.start_upload();
                            } else {
                                self.log("Upload cancelled - Overwrite required.");
                                self.status = "Upload Cancelled".to_string();
                            }
                            self.show_archive_overwrite_dialog = false; 
                        }
                        if ui.button(tr(lang, "cancel")).clicked() { 
                            self.show_archive_overwrite_dialog = false; 
                            self.status = "Connected".to_string(); 
                        }
                    });
                });
        }

        if self.show_override_dialog {
            egui::Window::new(tr(lang, "confirm_overwrite"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Folder already exists:\n{}

Overwrite it?", self.get_dest_path()));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "overwrite")).clicked() { self.start_upload(); }
                        if ui.button(tr(lang, "cancel")).clicked() { self.show_override_dialog = false; self.status = "Connected".to_string(); }
                    });
                });
        }

        if self.show_resume_dialog {
            egui::Window::new(tr(lang, "confirm_resume"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let mode_label = match self.config.resume_mode.as_str() {
                        "size" => tr(lang, "resume_fast"),
                        "size_mtime" => tr(lang, "resume_medium"),
                        "sha256" => tr(lang, "resume_slow"),
                        _ => tr(lang, "resume"),
                    };
                    ui.label(format!("Destination already exists:\n{}\n\n{}", self.get_dest_path(), mode_label));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "resume")).clicked() {
                            self.start_upload();
                        }
                        if ui.button(tr(lang, "overwrite")).clicked() {
                            self.force_full_upload_once = true;
                            self.start_upload();
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_resume_dialog = false;
                            self.status = "Connected".to_string();
                        }
                    });
                });
        }

        if self.show_archive_confirm_dialog {
            let archive_kind = self.pending_archive_kind.clone().unwrap_or_else(|| "Archive".to_string());
            egui::Window::new(tr(lang, "confirm_archive"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let detected_label = tr(lang, "archive_detected").replace("{}", &archive_kind);
                    let is_rar = archive_kind.eq_ignore_ascii_case("RAR");
                    ui.label(detected_label);
                    ui.add_space(6.0);
                    if is_rar {
                        // RAR files are extracted server-side on PS5
                        ui.label(egui::RichText::new("RAR will be uploaded and extracted on PS5.").weak());
                    } else {
                        ui.label(egui::RichText::new(tr(lang, "archive_extract_stream_note")).weak());
                    }
                    if self.config.connections > 1 && !is_rar {
                        ui.add_space(4.0);
                        ui.label(tr(lang, "archive_rar_stream_note"));
                    }
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.pending_archive_trim, tr(lang, "archive_trim_dir"));
                    if self.pending_archive_trim && !self.custom_subfolder.is_empty() {
                        self.custom_subfolder.clear();
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "continue")).clicked() {
                            if let Some(path) = self.pending_archive_path.take() {
                                self.update_game_path(path);
                                let msg = if is_rar {
                                    format!("RAR archive selected. Will be extracted on PS5.")
                                } else {
                                    tr(lang, "archive_selected_stream").replace("{}", &archive_kind)
                                };
                                self.log(&msg);
                            }
                            self.pending_archive_kind = None;
                            self.show_archive_confirm_dialog = false;
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.pending_archive_path = None;
                            self.pending_archive_kind = None;
                            self.pending_archive_trim = true;
                            self.show_archive_confirm_dialog = false;
                        }
                    });
                });
        }

        if self.manage_dest_open {
            egui::Window::new(tr(lang, "select_destination"))
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.heading(tr(lang, "select_destination"));
                    ui.add_space(4.0);
                    ui.label(tr(lang, "destination_path"));
                    ui.horizontal(|ui| {
                        let buttons_width = 3.0 * 70.0 + 12.0;
                        let path_width = (ui.available_width() - buttons_width).max(200.0);
                        ui.add_sized([path_width, 24.0], egui::TextEdit::singleline(&mut self.manage_right_path));
                        if ui.button(tr(lang, "go")).clicked() {
                            self.manage_refresh(ManageSide::Right);
                        }
                        if ui.button(tr(lang, "up")).clicked() {
                            let current_path = self.manage_right_path.clone();
                            if let Some(parent) = std::path::Path::new(&current_path).parent() {
                                self.manage_right_path = parent.display().to_string();
                                self.manage_refresh(ManageSide::Right);
                            }
                        }
                        if ui.button(tr(lang, "refresh")).clicked() {
                            self.manage_refresh(ManageSide::Right);
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr(lang, "breadcrumb"));
                        let mut parts = Vec::new();
                        let mut current = String::new();
                        let active_path = &self.manage_right_path;
                        if active_path.starts_with('/') {
                            parts.push("/".to_string());
                        }
                        for part in active_path.split('/').filter(|p| !p.is_empty()) {
                            if current.ends_with('/') || current.is_empty() {
                                current.push_str(part);
                            } else {
                                current.push('/');
                                current.push_str(part);
                            }
                            parts.push(current.clone());
                        }

                        for (idx, part) in parts.iter().enumerate() {
                            let label = if idx == 0 && part == "/" { "/" } else { part.rsplit('/').next().unwrap_or(part) };
                            if ui.button(label).clicked() {
                                self.manage_right_path = part.clone();
                                self.manage_refresh(ManageSide::Right);
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(self.manage_right_status.clone());

                    let mut open_dir: Option<String> = None;
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (idx, entry) in self.manage_right_entries.iter().enumerate() {
                            if entry.entry_type != "dir" {
                                continue;
                            }
                            let is_selected = self.manage_right_selected == Some(idx);
                            let response = ui.add(egui::SelectableLabel::new(
                                is_selected,
                                format!("📁 {}", entry.name),
                            ));
                            if response.clicked() {
                                self.manage_right_selected = Some(idx);
                            }
                            if response.double_clicked() {
                                open_dir = Some(entry.name.clone());
                            }
                        }
                    });

                    if let Some(dir_name) = open_dir {
                        self.manage_right_path = Self::join_remote_path(&self.manage_right_path, &dir_name);
                        self.manage_refresh(ManageSide::Right);
                    }

                    let selected_dest = self.manage_right_selected
                        .and_then(|idx| self.manage_right_entries.get(idx))
                        .filter(|e| e.entry_type == "dir")
                        .map(|e| Self::join_remote_path(&self.manage_right_path, &e.name))
                        .unwrap_or_else(|| self.manage_right_path.clone());

                    ui.add_space(6.0);
                    ui.label(format!("{} {}", tr(lang, "destination"), selected_dest));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "select_here")).clicked() {
                            if let (Some(action), Some(src), Some(name)) = (
                                self.manage_dest_action,
                                self.manage_dest_source_path.clone(),
                                self.manage_dest_source_name.clone(),
                            ) {
                                let dst = Self::join_remote_path(&selected_dest, &name);
                                match action {
                                    ManageDestAction::Move => {
                                        self.start_move_check(src, dst, tr(lang, "move").to_string());
                                    }
                                    ManageDestAction::Copy => {
                                        let ip = self.ip.clone();
                                        let rt = self.rt.clone();
                                        self.manage_send_op("Copy", move || {
                                            rt.block_on(async {
                                                copy_path(&ip, TRANSFER_PORT, &src, &dst).await
                                            }).map_err(|e| e.to_string())
                                        });
                                    }
                                }
                            }
                            self.manage_dest_open = false;
                            self.manage_dest_action = None;
                            self.manage_dest_source_path = None;
                            self.manage_dest_source_name = None;
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.manage_dest_open = false;
                            self.manage_dest_action = None;
                            self.manage_dest_source_path = None;
                            self.manage_dest_source_name = None;
                        }
                    });
                });
        }

        if self.show_update_restart_dialog {
            egui::Window::new(tr(lang, "update_restart_title"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(lang, "update_restart_note"));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "update_restart_now")).clicked() {
                            self.show_update_restart_dialog = false;
                            self.apply_self_update();
                        }
                        if ui.button(tr(lang, "update_restart_later")).clicked() {
                            self.show_update_restart_dialog = false;
                        }
                    });
                });
        }

        if self.show_download_overwrite_dialog {
            let request = self.pending_download_request.clone();
            let dest_label = match &request {
                Some(DownloadRequest::File { save_path, .. }) => save_path.clone(),
                Some(DownloadRequest::Dir { dest_root, .. }) => dest_root.clone(),
                None => String::new(),
            };
            egui::Window::new(tr(lang, "confirm_download"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Destination already exists:\n{}\n\nOverwrite and download?", dest_label));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "download")).clicked() {
                            self.show_download_overwrite_dialog = false;
                            if let Some(req) = request {
                                self.pending_download_request = None;
                                self.execute_download_request(req);
                            }
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_download_overwrite_dialog = false;
                            self.pending_download_request = None;
                        }
                    });
                });
        }

        if self.show_move_overwrite_dialog {
            let request = self.pending_move_request.clone();
            let (dest_label, src_label, exists_label) = match &request {
                Some(r) => (
                    r.dst.clone(),
                    r.src.clone(),
                    if r.dst_exists { "Destination already exists." } else { "Destination does not exist yet." },
                ),
                None => (String::new(), String::new(), ""),
            };
            egui::Window::new(tr(lang, "confirm_move"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Move from:\n{}\n\nMove to:\n{}\n\n{}", src_label, dest_label, exists_label));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "move")).clicked() {
                            self.show_move_overwrite_dialog = false;
                            if let Some(req) = request {
                                self.pending_move_request = None;
                                self.start_move_request(req);
                            }
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_move_overwrite_dialog = false;
                            self.pending_move_request = None;
                        }
                    });
                });
        }

        if self.show_rename_confirm {
            let request = self.pending_rename_request.clone();
            let (src_label, dst_label) = match &request {
                Some(r) => (r.src.clone(), r.dst.clone()),
                None => (String::new(), String::new()),
            };
            egui::Window::new(tr(lang, "confirm_rename"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Rename from:\n{}\n\nRename to:\n{}", src_label, dst_label));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "rename")).clicked() {
                            self.show_rename_confirm = false;
                            if let Some(req) = request {
                                self.pending_rename_request = None;
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("Rename", move || {
                                    rt.block_on(async {
                                        move_path(&ip, TRANSFER_PORT, &req.src, &req.dst).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_rename_confirm = false;
                            self.pending_rename_request = None;
                        }
                    });
                });
        }

        if self.show_delete_confirm {
            let target = self.pending_delete_target.clone().unwrap_or_default();
            egui::Window::new(tr(lang, "confirm_delete"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Delete:\n{}\n\nThis cannot be undone.", target));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "delete_action")).clicked() {
                            self.show_delete_confirm = false;
                            if let Some(target) = self.pending_delete_target.take() {
                                let ip = self.ip.clone();
                                let rt = self.rt.clone();
                                self.manage_send_op("Delete", move || {
                                    rt.block_on(async {
                                        delete_path(&ip, TRANSFER_PORT, &target).await
                                    }).map_err(|e| e.to_string())
                                });
                            }
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_delete_confirm = false;
                            self.pending_delete_target = None;
                        }
                    });
                });
        }

        if self.show_history_resume_dialog {
            let record = self.pending_history_record.clone();
            let dest_label = record.as_ref().map(|r| r.dest_path.clone()).unwrap_or_default();
            let src_label = record.as_ref().map(|r| r.source_path.clone()).unwrap_or_default();
            egui::Window::new(tr(lang, "resume_title"))
                .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("{}:\n{}\n\n{}:\n{}", tr(lang, "source_folder"), src_label, tr(lang, "destination"), dest_label));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "resume_mode"));
                        egui::ComboBox::from_id_source("history_resume_mode_combo")
                            .selected_text(match self.history_resume_mode.as_str() {
                                "size" => tr(lang, "resume_fast"),
                                "size_mtime" => tr(lang, "resume_medium"),
                                "sha256" => tr(lang, "resume_slow"),
                                _ => tr(lang, "off"),
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(self.history_resume_mode == "size", tr(lang, "resume_fast")).clicked() {
                                    self.history_resume_mode = "size".to_string();
                                }
                                if ui.selectable_label(self.history_resume_mode == "size_mtime", tr(lang, "resume_medium")).clicked() {
                                    self.history_resume_mode = "size_mtime".to_string();
                                }
                                if ui.selectable_label(self.history_resume_mode == "sha256", tr(lang, "resume_slow")).clicked() {
                                    self.history_resume_mode = "sha256".to_string();
                                }
                                if ui.selectable_label(self.history_resume_mode == "none", tr(lang, "off")).clicked() {
                                    self.history_resume_mode = "none".to_string();
                                }
                            });
                    });
                    ui.add_space(8.0);
                    let note = tr(lang, "note_resume");
                    ui.label(self.note_text(&note));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "resume")).clicked() {
                            self.show_history_resume_dialog = false;
                            if let Some(record) = record {
                                if let Err(msg) = self.apply_history_record(&record) {
                                    self.log(&format!("Resume failed: {}", msg));
                                } else {
                                    self.config.resume_mode = self.history_resume_mode.clone();
                                    let _ = self.config.save();
                                    self.auto_resume_on_exists = true;
                                    self.check_exists_and_upload();
                                }
                            }
                            self.pending_history_record = None;
                        }
                        if ui.button(tr(lang, "cancel")).clicked() {
                            self.show_history_resume_dialog = false;
                            self.pending_history_record = None;
                        }
                    });
                });
        }

        if self.config.auto_connect && !self.is_connected && !self.is_connecting && !self.is_uploading && !self.is_sending_payload {
            let now = std::time::Instant::now();
            let should_attempt = self.last_auto_connect_attempt
                .map(|last| now.duration_since(last).as_secs() >= 5)
                .unwrap_or(true);
            if should_attempt {
                self.last_auto_connect_attempt = Some(now);
                self.connect();
            }
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

                    ui.heading(tr(lang, "save_current_settings"));
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "profile_name"));
                        ui.text_edit_singleline(&mut self.profile_name_input);
                    });
                    ui.add_space(5.0);

                    ui.horizontal(|ui| {
                        let can_save = !self.profile_name_input.trim().is_empty();
                        if ui.add_enabled(can_save, egui::Button::new(tr(lang, "save_profile"))).clicked() {
                            let name = self.profile_name_input.trim().to_string();
                            self.save_current_as_profile(name);
                            self.profile_name_input.clear();
                        }
                        if ui.button(tr(lang, "close")).clicked() {
                            self.show_profile_dialog = false;
                        }
                    });
                });
        }

        // 1. TOP HEADER
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.logo_texture {
                        ui.add(egui::Image::new(tex).fit_to_exact_size([80.0, 80.0].into()));
                        ui.add_space(12.0);
                    }
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("PS5Upload").strong().size(22.0));
                        ui.label(
                            egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .size(14.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
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
            ui.add_space(6.0);
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

        self.autosave_profile_if_needed();

        // 3. LEFT PANEL: CONNECTION & STORAGE
        egui::SidePanel::left("left_panel").resizable(true).default_width(300.0).min_width(250.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.heading(tr(lang, "connect"));
                });
                ui.add_space(5.0);

            // Profile selector
            ui.horizontal(|ui| {
                ui.label(tr(lang, "profile"));
                let profile_names: Vec<String> = self.profiles_data.profiles.iter().map(|p| p.name.clone()).collect();
                egui::ComboBox::from_id_source("profile_combo")
                    .selected_text(self.current_profile.as_deref().unwrap_or("(none)"))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(self.current_profile.is_none(), "(none)").clicked() {
                            self.current_profile = None;
                            self.set_default_profile(None);
                        }
                        for name in &profile_names {
                            if ui.selectable_label(self.current_profile.as_ref() == Some(name), name).clicked() {
                                if let Some(profile) = self.profiles_data.profiles.iter().find(|p| &p.name == name) {
                                    let profile = profile.clone();
                                    self.apply_profile(&profile);
                                    self.set_default_profile(Some(profile.name.clone()));
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

            ui.label(tr(lang, "ps5_address"));
            ui.text_edit_singleline(&mut self.ip);
            ui.label(format!("Transfer port: {}", TRANSFER_PORT));

            ui.add_space(8.0);
            ui.label(tr(lang, "payload"));
            ui.label(tr(lang, "payload_required"));
            ui.label(format!("{} {}", tr(lang, "payload_port"), PAYLOAD_PORT));
            ui.horizontal(|ui| {
                if ui.button(format!("📂 {}", tr(lang, "select"))).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Payload", &["elf"]).pick_file() {
                        self.payload_path = path.display().to_string();
                    }
                }
                ui.text_edit_singleline(&mut self.payload_path);
            });
            ui.add_space(5.0);
            let payload_enabled = !self.is_sending_payload && !self.ip.trim().is_empty() && !self.payload_path.trim().is_empty();
            if ui.add_enabled(payload_enabled, egui::Button::new(format!("📤 {}", tr(lang, "send_payload"))).min_size([ui.available_width(), 30.0].into())).clicked() {
                self.send_payload();
            }
            ui.add_space(5.0);
            ui.label(format!("{} {}", tr(lang, "payload_status"), self.payload_status));
            if ui.button(tr(lang, "check_payload_status")).clicked() {
                self.check_payload_version();
            }
            ui.add_space(5.0);
            if ui.button(tr(lang, "download_current_payload")).clicked() {
                self.start_payload_download_and_send(PayloadFetch::Current);
            }
            if ui.button(tr(lang, "download_latest_payload")).clicked() {
                self.start_payload_download_and_send(PayloadFetch::Latest);
            }
            ui.add_space(5.0);
            let auto_payload = ui.checkbox(&mut self.config.auto_check_payload, tr(lang, "auto_check_payload"));
            if auto_payload.changed() {
                let _ = self.config.save();
                if self.config.auto_check_payload {
                    self.last_payload_check = None;
                    self.check_payload_version();
                }
            }
            if self.config.auto_check_payload {
                ui.label(egui::RichText::new(tr(lang, "auto_payload_interval")).weak().small());
            }
            let auto_connect = ui.checkbox(&mut self.config.auto_connect, tr(lang, "auto_reconnect"));
            if auto_connect.changed() { let _ = self.config.save(); }

            ui.add_space(10.0);
            if connected {
                if ui.add(egui::Button::new(tr(lang, "disconnect")).min_size([ui.available_width(), 30.0].into())).clicked() {
                    self.status = "Disconnected".to_string();
                    self.storage_locations.clear();
                    self.selected_storage = None;
                    self.is_connected = false;
                    self.last_payload_check = None;
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.is_connecting, egui::Button::new(tr(lang, "connect_ps5")).min_size([150.0, 30.0].into())).clicked() {
                        self.connect();
                    }
                    if self.is_connecting
                        && ui.add(egui::Button::new(tr(lang, "stop")).min_size([60.0, 30.0].into())).clicked()
                    {
                        self.is_connecting = false;
                        self.status = "Cancelled".to_string();
                    }
                });
            }
            
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.heading(tr(lang, "storage"));
                if connected && ui.button(format!("⟳ {}", tr(lang, "refresh"))).clicked() {
                    self.connect();
                }
            });
            ui.add_space(5.0);
            
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.storage_locations.is_empty() {
                    ui.label(tr(lang, "not_connected"));
                } else {
                     egui::Grid::new("storage_grid").striped(true).spacing([10.0, 5.0]).show(ui, |ui| {
                         for loc in &self.storage_locations {
                            if ui.radio_value(&mut self.selected_storage, Some(loc.path.clone()), &loc.path).clicked() {
                                self.config.storage = loc.path.clone();
                                let _ = self.config.save();
                            }
                            ui.label(format!("{:.1} GB Free", loc.free_gb));
                            ui.end_row();
                         }
                     });
                }
            });

            ui.add_space(15.0);
            ui.heading(tr(lang, "updates"));
            ui.add_space(5.0);
            ui.label(format!("Current: v{}", app_version_trimmed()));
            if self.update_available {
                ui.label(egui::RichText::new("New version available").color(egui::Color32::from_rgb(255, 140, 0)));
            }
            ui.label(self.update_status.clone());
            if ui.button(tr(lang, "check_updates")).clicked() {
                self.start_update_check();
            }
            let mut include_pre = self.config.update_channel == "all";
            if ui.checkbox(&mut include_pre, tr(lang, "include_prerelease")).changed() {
                self.config.update_channel = if include_pre { "all".to_string() } else { "stable".to_string() };
                let _ = self.config.save();
                self.start_update_check();
            }

            if let Some(info) = &self.update_info {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button(tr(lang, "open_release_page")).clicked() {
                        let _ = webbrowser::open(&info.html_url);
                    }
                });

                ui.add_space(5.0);
                if ui.button(tr(lang, "download_payload")).clicked() {
                    self.start_download_asset("payload", "ps5upload.elf", "ps5upload.elf");
                }

                match current_asset_name() {
                    Ok(asset_name) => {
                        if ui.button(tr(lang, "download_client")).clicked() {
                            self.start_download_asset("client", &asset_name, &asset_name);
                        }
                        if self.update_available {
                            if self.pending_update.is_some() {
                                if ui.button(tr(lang, "update_restart")).clicked() {
                                    self.show_update_restart_dialog = true;
                                }
                            } else if ui.button(tr(lang, "update_now")).clicked() {
                                self.start_self_update();
                            }
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

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(tr(lang, "language"));
                egui::ComboBox::from_id_source("language_combo")
                    .selected_text(self.language.label())
                    .show_ui(ui, |ui| {
                        for candidate in [
                            Language::En,
                            Language::ZhCn,
                            Language::ZhTw,
                            Language::Fr,
                            Language::Es,
                            Language::Ar,
                        ] {
                            if ui.selectable_label(self.language == candidate, candidate.label()).clicked() {
                                self.language = candidate;
                                self.config.language = candidate.code().to_string();
                                let _ = self.config.save();
                            }
                        }
                    });
            });
            });
        });

        // 4. RIGHT PANEL: LOGS
        egui::SidePanel::right("right_panel").resizable(true).default_width(450.0).min_width(350.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.heading(tr(lang, "logs"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑 Clear").clicked() {
                            self.client_logs.clear();
                            self.payload_logs.clear();
                        }
                    });
                });
            
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.log_tab, 0, tr(lang, "client"));
                ui.selectable_value(&mut self.log_tab, 1, tr(lang, "payload"));
                ui.selectable_value(&mut self.log_tab, 2, tr(lang, "history"));
            });
            ui.separator();

            if self.log_tab == 2 {
                // History tab
                let records = self.history_data.records.clone();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr(lang, "clear_history")).clicked() {
                            clear_history(&mut self.history_data);
                        }
                    });
                });
                ui.add_space(5.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.history_data.records.is_empty() {
                        ui.label(tr(lang, "no_history"));
                    } else {
                        for record in records.iter().rev() {
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
                                ui.label(format!("{}: {}", tr(lang, "source_folder"), source_name));
                                ui.label(format!("{}: {}", tr(lang, "destination"), record.dest_path));
                                let transfer_kind = if record.via_queue { tr(lang, "queue_upload") } else { tr(lang, "single_upload") };
                                ui.label(self.note_text(&transfer_kind));

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

                                ui.add_space(4.0);
                                if ui.button(tr(lang, "resume_btn")).clicked() {
                                    self.pending_history_record = Some(record.clone());
                                    self.history_resume_mode = if self.config.resume_mode == "none" {
                                        "size".to_string()
                                    } else {
                                        self.config.resume_mode.clone()
                                    };
                                    self.show_history_resume_dialog = true;
                                }
                            });
                            ui.add_space(2.0);
                        }
                    }
                });

            } else {
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    let text = if self.log_tab == 0 { &self.client_logs } else { &self.payload_logs };
                    ui.add(egui::TextEdit::multiline(&mut text.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .cursor_at_end(false)
                        .interactive(false));
                });
            }
            });
        });

        // 5. CENTRAL PANEL: MAIN
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let transfer_tab = ui.selectable_value(&mut self.main_tab, 0, tr(lang, "transfer"));
                    let manage_tab = ui.selectable_value(&mut self.main_tab, 1, tr(lang, "manage"));
                    let chat_tab = ui.selectable_value(&mut self.main_tab, 2, tr(lang, "chat"));
                    if manage_tab.clicked() && connected {
                        self.manage_refresh(ManageSide::Left);
                    }
                    if transfer_tab.clicked() || chat_tab.clicked() {
                        self.manage_left_selected = None;
                        self.manage_right_selected = None;
                    }
                });
                ui.separator();

            if self.main_tab == 1 {
                ui.heading(tr(lang, "file_manager"));
                ui.add_space(8.0);

                let can_manage = connected && !self.manage_busy && !self.is_downloading;

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "path"));
                        let buttons_width = 3.0 * 70.0 + 12.0;
                        let path_width = (ui.available_width() - buttons_width).max(200.0);
                        ui.add_sized([path_width, 24.0], egui::TextEdit::singleline(&mut self.manage_left_path));
                        if ui.add_enabled(can_manage, egui::Button::new(tr(lang, "go"))).clicked() {
                            self.manage_refresh(ManageSide::Left);
                        }
                        if ui.add_enabled(can_manage, egui::Button::new(tr(lang, "up"))).clicked() {
                            let current_path = self.manage_left_path.clone();
                            if let Some(parent) = std::path::Path::new(&current_path).parent() {
                                self.manage_left_path = parent.display().to_string();
                                self.manage_refresh(ManageSide::Left);
                            }
                        }
                        if ui.add_enabled(can_manage, egui::Button::new(tr(lang, "refresh"))).clicked() {
                            self.manage_refresh(ManageSide::Left);
                        }
                    });

                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr(lang, "breadcrumb"));
                        let mut parts = Vec::new();
                        let mut current = String::new();
                        let active_path = &self.manage_left_path;
                        if active_path.starts_with('/') {
                            parts.push("/".to_string());
                        }
                        for part in active_path.split('/').filter(|p| !p.is_empty()) {
                            if current.ends_with('/') || current.is_empty() {
                                current.push_str(part);
                            } else {
                                current.push('/');
                                current.push_str(part);
                            }
                            parts.push(current.clone());
                        }

                        for (idx, part) in parts.iter().enumerate() {
                            let label = if idx == 0 && part == "/" { "/" } else { part.rsplit('/').next().unwrap_or(part) };
                            if ui.add_enabled(can_manage, egui::Button::new(label)).clicked() {
                                self.manage_left_path = part.clone();
                                self.manage_refresh(ManageSide::Left);
                            }
                        }
                    });

                    ui.add_space(4.0);
                    ui.label(self.manage_left_status.clone());
                });

                ui.add_space(10.0);

                let total_height = ui.available_height();
                let list_height = (total_height * 0.55).clamp(240.0, 520.0);
                let _details_height = (total_height - list_height - 12.0).max(240.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(tr(lang, "items"));
                    if !connected {
                        ui.label(tr(lang, "not_connected"));
                    } else {
                        let mut open_dir: Option<String> = None;
                        let entries = &self.manage_left_entries;
                        let selected_ref = &mut self.manage_left_selected;
                        egui::ScrollArea::vertical().max_height(list_height).show(ui, |ui| {
                            let total_width = ui.available_width();
                            let name_w = (total_width * 0.45).max(220.0);
                            let type_w = (total_width * 0.18).max(120.0);
                            let size_w = (total_width * 0.12).max(90.0);
                            let modified_w = (total_width - name_w - type_w - size_w).max(160.0);
                            let row_height = ui.spacing().interact_size.y;

                            ui.scope(|ui| {
                                let base = ui.visuals().selection.bg_fill;
                                let strong = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 120);
                                ui.visuals_mut().selection.bg_fill = strong;
                                ui.visuals_mut().selection.stroke = egui::Stroke::new(1.0, base);

                                egui::Grid::new("manage_list").striped(true).spacing([0.0, 4.0]).show(ui, |ui| {
                                    ui.add_sized([name_w, row_height], egui::Label::new(egui::RichText::new(tr(lang, "name")).strong()));
                                    ui.add_sized([type_w, row_height], egui::Label::new(egui::RichText::new(tr(lang, "type")).strong()));
                                    ui.add_sized([size_w, row_height], egui::Label::new(egui::RichText::new(tr(lang, "size")).strong()));
                                    ui.add_sized([modified_w, row_height], egui::Label::new(egui::RichText::new(tr(lang, "modified")).strong()));
                                    ui.end_row();

                                    for (idx, entry) in entries.iter().enumerate() {
                                        let is_selected = *selected_ref == Some(idx);
                                        let icon = if entry.entry_type == "dir" { "📁" } else { "📄" };
                                        let mut row_clicked = false;
                                        let mut row_double_clicked = false;
                                        let response = ui.add_sized(
                                            [name_w, row_height],
                                            egui::SelectableLabel::new(is_selected, format!("{} {}", icon, entry.name)),
                                        );
                                        if response.clicked() {
                                            row_clicked = true;
                                        }
                                        if response.double_clicked() {
                                            row_double_clicked = true;
                                        }
                                        let type_label = if entry.entry_type == "dir" { tr(lang, "directory") } else { tr(lang, "file") };
                                        let type_response = ui.add_sized(
                                            [type_w, row_height],
                                            egui::SelectableLabel::new(is_selected, type_label),
                                        );
                                        if type_response.clicked() {
                                            row_clicked = true;
                                        }
                                        if type_response.double_clicked() {
                                            row_double_clicked = true;
                                        }
                                        let size_label = if entry.entry_type == "dir" {
                                            "--".to_string()
                                        } else {
                                            format_bytes(entry.size)
                                        };
                                        let size_response = ui.add_sized(
                                            [size_w, row_height],
                                            egui::SelectableLabel::new(is_selected, size_label),
                                        );
                                        if size_response.clicked() {
                                            row_clicked = true;
                                        }
                                        if size_response.double_clicked() {
                                            row_double_clicked = true;
                                        }
                                        let modified_label = format_modified_time(entry.mtime);
                                        let modified_response = ui.add_sized(
                                            [modified_w, row_height],
                                            egui::SelectableLabel::new(is_selected, modified_label),
                                        );
                                        if modified_response.clicked() {
                                            row_clicked = true;
                                        }
                                        if modified_response.double_clicked() {
                                            row_double_clicked = true;
                                        }

                                        if is_selected {
                                            let row_rect = response
                                                .rect
                                                .union(type_response.rect)
                                                .union(size_response.rect)
                                                .union(modified_response.rect);
                                            ui.painter().rect_stroke(
                                                row_rect,
                                                0.0,
                                                ui.visuals().selection.stroke,
                                            );
                                        }

                                        if row_clicked {
                                            *selected_ref = Some(idx);
                                            self.manage_new_name = entry.name.clone();
                                        }
                                        if row_double_clicked && entry.entry_type == "dir" {
                                            open_dir = Some(entry.name.clone());
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                        });
                        if let Some(dir_name) = open_dir {
                            self.manage_left_path = Self::join_remote_path(&self.manage_left_path, &dir_name);
                            self.manage_refresh(ManageSide::Left);
                        }
                    }
                });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(tr(lang, "details_actions"));

                    let selected_info = self.manage_left_selected
                        .and_then(|idx| self.manage_left_entries.get(idx))
                        .map(|entry| (entry.name.clone(), entry.entry_type.clone(), entry.size, entry.mtime));

                    let mut selected_name: Option<String> = None;
                    let mut selected_type: Option<String> = None;
                    let mut selected_path: Option<String> = None;
                    let mut selected_size: Option<u64> = None;
                    let mut selected_mtime: Option<Option<i64>> = None;

                    if let Some((name, entry_type, size, mtime)) = selected_info {
                        let path = Self::join_remote_path(&self.manage_left_path, &name);
                        selected_name = Some(name);
                        selected_type = Some(entry_type);
                        selected_path = Some(path);
                        selected_size = Some(size);
                        selected_mtime = Some(mtime);
                    }

                    if let (Some(ref name), Some(ref entry_type), Some(path)) = (selected_name.clone(), selected_type.clone(), selected_path.clone()) {
                        egui::Grid::new("manage_details").spacing([10.0, 6.0]).show(ui, |ui| {
                            ui.label(tr(lang, "name")); ui.label(name); ui.end_row();
                            ui.label(tr(lang, "type")); ui.label(if entry_type == "dir" { tr(lang, "directory") } else { tr(lang, "file") }); ui.end_row();
                            if entry_type == "file" {
                                if let Some(size) = selected_size {
                                    ui.label(tr(lang, "size")); ui.label(format_bytes(size)); ui.end_row();
                                }
                            }
                            ui.label(tr(lang, "modified")); ui.label(format_modified_time(selected_mtime.flatten())); ui.end_row();
                            ui.label(tr(lang, "path")); ui.label(path.clone()); ui.end_row();
                        });
                    } else {
                        ui.label(tr(lang, "select_item"));
                    }

                    let can_ops = connected && !self.manage_busy && !self.is_downloading;
                    let has_selection = selected_name.is_some() && selected_path.is_some() && selected_type.is_some();

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "rename"));
                        ui.text_edit_singleline(&mut self.manage_new_name);
                    });

                    ui.horizontal(|ui| {
                        let can_rename = can_ops
                            && has_selection
                            && !self.manage_new_name.trim().is_empty()
                            && self.manage_new_name.trim() != selected_name.as_deref().unwrap_or("");
                        if ui.add_enabled(can_rename, egui::Button::new(tr(lang, "rename"))).clicked() {
                            let src = selected_path.clone().unwrap_or_default();
                            let new_name = self.manage_new_name.trim().to_string();
                            let dst = Self::join_remote_path(&self.manage_left_path, &new_name);
                            self.pending_rename_request = Some(RenameRequest { src, dst });
                            self.show_rename_confirm = true;
                        }
                        let can_new_folder = can_ops && !self.manage_new_name.trim().is_empty();
                        if ui.add_enabled(can_new_folder, egui::Button::new(tr(lang, "new_folder"))).clicked() {
                            let folder_name = self.manage_new_name.trim().to_string();
                            let full_path = Self::join_remote_path(&self.manage_left_path, &folder_name);
                            let ip = self.ip.clone();
                            let rt = self.rt.clone();
                            self.manage_send_op("New folder", move || {
                                rt.block_on(async {
                                    create_path(&ip, TRANSFER_PORT, &full_path).await
                                }).map_err(|e| e.to_string())
                            });
                            self.manage_new_name.clear();
                        }
                        if ui.add_enabled(can_ops && has_selection, egui::Button::new(tr(lang, "move"))).clicked() {
                            let src = selected_path.clone().unwrap_or_default();
                            let name = selected_name.clone().unwrap_or_default();
                            self.open_destination_picker(ManageDestAction::Move, src, name);
                        }
                        if ui.add_enabled(can_ops && has_selection, egui::Button::new(tr(lang, "copy"))).clicked() {
                            let src = selected_path.clone().unwrap_or_default();
                            let name = selected_name.clone().unwrap_or_default();
                            self.open_destination_picker(ManageDestAction::Copy, src, name);
                        }
                        let can_download = can_ops && has_selection;
                        if ui.add_enabled(can_download, egui::Button::new(tr(lang, "download"))).clicked() {
                            let name = selected_name.clone().unwrap_or_default();
                            let entry_type = selected_type.clone().unwrap_or_default();
                            let target = selected_path.clone().unwrap_or_default();
                            let mut compression = match self.config.download_compression.as_str() {
                                "lz4" => DownloadCompression::Lz4,
                                "zstd" => DownloadCompression::Zstd,
                                "lzma" => DownloadCompression::Lzma,
                                "auto" => DownloadCompression::Auto,
                                _ => DownloadCompression::None,
                            };
                            if matches!(compression, DownloadCompression::Zstd | DownloadCompression::Lzma | DownloadCompression::Auto)
                                && !self.payload_supports_modern_compression()
                            {
                                self.log("Payload does not support Zstd/LZMA yet; using LZ4 for download.");
                                compression = DownloadCompression::Lz4;
                            }
                            if entry_type == "file" {
                                if let Some(save_path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                                    let save_path = save_path.display().to_string();
                                    let request = DownloadRequest::File { name, target, save_path: save_path.clone() };
                                    if std::path::Path::new(&save_path).exists() {
                                        self.pending_download_request = Some(request);
                                        self.show_download_overwrite_dialog = true;
                                    } else {
                                        self.execute_download_request(request);
                                    }
                                }
                            } else if let Some(save_folder) = rfd::FileDialog::new().pick_folder() {
                                let dest_root = save_folder.join(&name).display().to_string();
                                let request = DownloadRequest::Dir { name, target, dest_root: dest_root.clone(), compression };
                                if std::path::Path::new(&dest_root).exists() {
                                    self.pending_download_request = Some(request);
                                    self.show_download_overwrite_dialog = true;
                                } else {
                                    self.execute_download_request(request);
                                }
                            }
                        }
                        if ui.add_enabled(can_ops && has_selection, egui::Button::new(tr(lang, "delete"))).clicked() {
                            let target = selected_path.clone().unwrap_or_default();
                            self.pending_delete_target = Some(target);
                            self.show_delete_confirm = true;
                        }
                        if ui.add_enabled(can_ops && has_selection, egui::Button::new(tr(lang, "chmod")))
                            .on_hover_text("Applies recursively")
                            .clicked() {
                            let target = selected_path.clone().unwrap_or_default();
                            let ip = self.ip.clone();
                            let rt = self.rt.clone();
                            self.manage_send_op("chmod", move || {
                                rt.block_on(async {
                                    chmod_777(&ip, TRANSFER_PORT, &target).await
                                }).map_err(|e| e.to_string())
                            });
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(tr(lang, "download_progress"));
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "compression"));
                        egui::ComboBox::from_id_source("download_compression_combo")
                            .selected_text(match self.config.download_compression.as_str() {
                                "auto" => tr(lang, "compression_opt_auto"),
                                "lz4" => tr(lang, "compression_opt_lz4"),
                                "zstd" => tr(lang, "compression_opt_zstd"),
                                "lzma" => tr(lang, "compression_opt_lzma"),
                                _ => tr(lang, "compression_opt_none"),
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(self.config.download_compression == "none", tr(lang, "compression_opt_none")).clicked() {
                                    self.config.download_compression = "none".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.download_compression == "auto", tr(lang, "compression_opt_auto")).clicked() {
                                    self.config.download_compression = "auto".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.download_compression == "lz4", tr(lang, "compression_opt_lz4")).clicked() {
                                    self.config.download_compression = "lz4".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.download_compression == "zstd", tr(lang, "compression_opt_zstd")).clicked() {
                                    self.config.download_compression = "zstd".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.download_compression == "lzma", tr(lang, "compression_opt_lzma")).clicked() {
                                    self.config.download_compression = "lzma".to_string();
                                    let _ = self.config.save();
                                }
                            });
                    });
                    let total = self.download_progress_total.max(1);
                    let progress = (self.download_progress_sent as f64 / total as f64).clamp(0.0, 1.0) as f32;
                    ui.add(egui::ProgressBar::new(progress).show_percentage().animate(self.is_downloading));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} / {}",
                            format_bytes(self.download_progress_sent),
                            format_bytes(self.download_progress_total)
                        ));
                        ui.separator();
                        ui.label(format!("{}/s", format_bytes(self.download_speed_bps as u64)));
                        ui.separator();
                        ui.label(format!("ETA {}", self.download_eta_secs.map(format_duration).unwrap_or("N/A".to_string())));
                    });
                    if !self.download_current_file.is_empty() {
                        ui.label(format!("Current file: {}", self.download_current_file));
                    }
                    if self.is_downloading
                        && ui.add(egui::Button::new(tr(lang, "stop_download")).min_size([140.0, 30.0].into())).clicked()
                    {
                        self.download_cancellation_token.store(true, Ordering::Relaxed);
                        self.log("Stopping download...");
                    }

                    if self.is_moving {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(tr(lang, "move_progress"));
                        ui.add(egui::ProgressBar::new(0.5).show_percentage().animate(true));
                        if ui.add(egui::Button::new(tr(lang, "stop_move")).min_size([140.0, 30.0].into())).clicked() {
                            self.move_cancellation_token.store(true, Ordering::Relaxed);
                            self.log("Stopping move...");
                        }
                    }
                });
            } else if self.main_tab == 2 {
                ui.heading(tr(lang, "chat"));
                ui.add_space(6.0);
                ui.label(format!("{} {}", tr(lang, "chat_status"), self.chat_status));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tr(lang, "chat_display_name"));
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.config.chat_display_name)
                            .desired_width(220.0),
                    );
                    if response.changed() {
                        let _ = self.config.save();
                    }
                });
                ui.label(egui::RichText::new(tr(lang, "chat_display_name_note")).weak().small());

                let remaining_height = ui.available_height().max(200.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), remaining_height), |ui| {
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.horizontal(|ui| {
                            let mut send_now = false;
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.chat_input)
                                    .hint_text(tr(lang, "chat_input_placeholder"))
                                    .desired_width(f32::INFINITY),
                            );
                            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                send_now = true;
                            }
                            let can_send = self.chat_tx.is_some() && !self.chat_input.trim().is_empty();
                            if ui.add_enabled(can_send, egui::Button::new(tr(lang, "chat_send"))).clicked() {
                                send_now = true;
                            }
                            if send_now {
                                self.send_chat_message();
                            }
                        });

                        ui.add_space(8.0);
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            let list_height = ui.available_height().max(120.0);
                            egui::ScrollArea::vertical().stick_to_bottom(true).max_height(list_height).show(ui, |ui| {
                                if self.chat_messages.is_empty() {
                                    ui.label(tr(lang, "chat_empty"));
                                } else {
                                    for msg in &self.chat_messages {
                                        let sender = if msg.local {
                                            tr(lang, "chat_you")
                                        } else {
                                            msg.sender.clone()
                                        };
                                        ui.label(format!("[{}] {}: {}", msg.time, sender, msg.text));
                                    }
                                }
                            });
                        });
                    });
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
                        } else if let Some(kind) = Self::archive_kind(path) {
                            self.prompt_archive_confirm(path, kind);
                        } else if let Some(parent) = path.parent() {
                            self.update_game_path(parent.display().to_string());
                        }
                    }
                }

                ui.add_space(10.0);
                ui.heading(tr(lang, "transfer"));
                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(tr(lang, "source_folder"));
                    ui.horizontal(|ui| {
                        if ui.button(format!("📂 {}", tr(lang, "browse"))).clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                self.update_game_path(path.display().to_string());
                            }
                        }
                        if ui.button(format!("📦 {}", tr(lang, "browse_archive"))).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Archives", &["zip", "7z", "rar"])
                                .pick_file()
                            {
                                if let Some(kind) = Self::archive_kind(&path) {
                                    self.prompt_archive_confirm(&path, kind);
                                }
                            }
                        }
                        let can_add_queue = !self.game_path.trim().is_empty() && !self.is_uploading;
                        if ui.add_enabled(can_add_queue, egui::Button::new(tr(lang, "add_queue"))).on_hover_text("Add to transfer queue").clicked() {
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
                    format!("{} ({})", tr(lang, "upload_queue"), pending_count)
                } else {
                    tr(lang, "upload_queue").to_string()
                };

                ui.add_space(5.0);
                egui::CollapsingHeader::new(queue_label)
                    .default_open(false)
                    .show(ui, |ui| {
                        if self.queue_data.items.is_empty() {
                            ui.label(tr(lang, "queue_empty"));
                        } else {
                            let mut item_to_remove: Option<u64> = None;

                            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                                for item in &self.queue_data.items {
                                    ui.vertical(|ui| {
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
                                                if item.status == QueueStatus::Pending
                                                    && ui.small_button("×").on_hover_text("Remove").clicked()
                                                {
                                                    item_to_remove = Some(item.id);
                                                }
                                            });
                                        });
                                        let base = self.selected_storage.as_deref().unwrap_or("/data");
                                        let dest = self.build_dest_path_for_item(base, item);
                                        ui.label(egui::RichText::new(format!("{}: {}", tr(lang, "destination"), dest)).weak().small());
                                    });
                                }
                            });

                            if let Some(id) = item_to_remove {
                                self.remove_from_queue(id);
                            }

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                let _has_pending = self.queue_data.items.iter().any(|i| i.status == QueueStatus::Pending);
                                if ui.button("Clear Completed").clicked() {
                                    self.clear_completed_queue();
                                }
                            });
                        }
                    });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(tr(lang, "destination_path"));
                    egui::Grid::new("dest_grid").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
                        ui.label(format!("{}:", tr(lang, "preset")));
                        egui::ComboBox::from_id_source("preset_combo").selected_text(PRESETS[self.selected_preset]).show_ui(ui, |ui| {
                            for (i, p) in PRESETS.iter().enumerate() { ui.selectable_value(&mut self.selected_preset, i, *p); }
                        });
                        ui.end_row();
                        if self.selected_preset == 2 { ui.label(format!("{}:", tr(lang, "path"))); ui.text_edit_singleline(&mut self.custom_preset_path); ui.end_row(); }
                        let is_archive = Self::archive_kind(Path::new(&self.game_path)).is_some();
                        let trim_active = is_archive && self.pending_archive_trim;
                        ui.label(format!("{}:", tr(lang, "name_label")));
                        ui.add_enabled_ui(!trim_active, |ui| {
                            ui.text_edit_singleline(&mut self.custom_subfolder);
                        });
                        ui.end_row();
                        if trim_active {
                            ui.label(egui::RichText::new(tr(lang, "archive_trim_ignores_name")).weak().small());
                            ui.end_row();
                        }
                        ui.label(format!("{}:", tr(lang, "use_temp")));
                        let temp_toggle = ui.checkbox(&mut self.config.use_temp, tr(lang, "stage_fast"));
                        if temp_toggle.changed() { let _ = self.config.save(); }
                        ui.end_row();
                        ui.label(tr(lang, "compression"));
                        ui.add_enabled_ui(!self.config.optimize_upload, |ui| {
                            egui::ComboBox::from_id_source("compression_combo")
                                .selected_text(match self.config.compression.as_str() {
                                    "auto" => tr(lang, "compression_opt_auto"),
                                    "lz4" => tr(lang, "compression_opt_lz4"),
                                    "zstd" => tr(lang, "compression_opt_zstd"),
                                    "lzma" => tr(lang, "compression_opt_lzma"),
                                    _ => tr(lang, "compression_opt_none"),
                                })
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(self.config.compression == "none", tr(lang, "compression_opt_none")).clicked() {
                                        self.config.compression = "none".to_string();
                                        let _ = self.config.save();
                                    }
                                    if ui.selectable_label(self.config.compression == "auto", tr(lang, "compression_opt_auto")).clicked() {
                                        self.config.compression = "auto".to_string();
                                        let _ = self.config.save();
                                    }
                                    if ui.selectable_label(self.config.compression == "lz4", tr(lang, "compression_opt_lz4")).clicked() {
                                        self.config.compression = "lz4".to_string();
                                        let _ = self.config.save();
                                    }
                                    if ui.selectable_label(self.config.compression == "zstd", tr(lang, "compression_opt_zstd")).clicked() {
                                        self.config.compression = "zstd".to_string();
                                        let _ = self.config.save();
                                    }
                                    if ui.selectable_label(self.config.compression == "lzma", tr(lang, "compression_opt_lzma")).clicked() {
                                        self.config.compression = "lzma".to_string();
                                        let _ = self.config.save();
                                    }
                                });
                        });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        let optimize = ui.checkbox(&mut self.config.optimize_upload, tr(lang, "optimize_upload_auto"));
                        if optimize.changed() {
                            let _ = self.config.save();
                            if self.config.optimize_upload {
                                self.log(&tr(lang, "optimize_upload_notice"));
                            }
                        }
                        if ui.button(tr(lang, "optimize_upload_now")).clicked() {
                            self.start_optimize_upload();
                        }
                    });
                    ui.label(egui::RichText::new("RAR archives are extracted on PS5.").weak().small());
                    if self.config.optimize_upload {
                        ui.label(egui::RichText::new(tr(lang, "optimize_upload_lock_notice")).weak().small());
                    }
                    ui.add_space(5.0);
                    ui.label(egui::RichText::new(format!("➡ Destination: {}", self.get_dest_path())).monospace().weak());
                });

                ui.add_space(10.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(tr(lang, "transfer_progress"));
                    ui.add_space(5.0);

                    if self.is_scanning && !self.is_uploading {
                        ui.label(format!("Phase: {}", tr(lang, "phase_scanning")));
                        let scan_progress = (self.scanning_files_found as f32 / 10_000.0).min(1.0);
                        ui.add(egui::ProgressBar::new(scan_progress).show_percentage().animate(true));
                        ui.add_space(5.0);
                        ui.label(format!("Scanning: {} files ({})", self.scanning_files_found, format_bytes(self.scanning_total_size)));
                        ui.label(tr(lang, "progress_total_estimated"));
                    } else {
                        let total = self.progress_total.max(1);
                        let progress = (self.progress_sent as f64 / total as f64).clamp(0.0, 1.0) as f32;
                        if !self.progress_phase.is_empty() {
                            let phase_label = match self.progress_phase.as_str() {
                                "Packing" => tr(lang, "phase_packing"),
                                "Sending" => tr(lang, "phase_sending"),
                                "Scanning" => tr(lang, "phase_scanning"),
                                "Extracting" => tr(lang, "phase_extracting"),
                                _ => self.progress_phase.clone(),
                            };
                            ui.label(format!("Phase: {}", phase_label));
                        }
                        ui.add(egui::ProgressBar::new(progress).show_percentage().animate(self.is_uploading));

                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            ui.label(format!("Sent: {} / {}", format_bytes(self.progress_sent), format_bytes(self.progress_total)));
                            ui.separator();
                            ui.label(format!("{}/s", format_bytes(self.progress_speed_bps as u64)));
                            ui.separator();
                            ui.label(format!("ETA {}", self.progress_eta_secs.map(format_duration).unwrap_or("N/A".to_string())));
                        });
                        
                        if self.progress_files > 0 { ui.label(format!("Files transferred: {}", self.progress_files)); }
                        if !self.progress_current_file.is_empty() {
                            ui.label(format!("Current file: {}", self.progress_current_file));
                        }
                        if self.progress_sent == 0 && self.scanning_files_found > 0 {
                            let scan_progress = (self.scanning_files_found as f32 / 10_000.0).min(1.0);
                            ui.label(format!("Scanning: {} files ({})", self.scanning_files_found, format_bytes(self.scanning_total_size)));
                            ui.add(egui::ProgressBar::new(scan_progress).show_percentage().animate(true));
                            ui.label(tr(lang, "progress_total_estimated"));
                        }
                    }
                    
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "connections"));
                        let mut connections = self.config.connections as i32;
                        let response = ui.add_enabled(
                            !self.config.optimize_upload,
                            egui::DragValue::new(&mut connections)
                                .clamp_range(1..=MAX_PARALLEL_CONNECTIONS as i32)
                                .speed(1.0)
                        );
                        if response.changed() {
                            self.config.connections = connections as usize;
                            let _ = self.config.save();
                        }
                        ui.label(format!("(max {})", MAX_PARALLEL_CONNECTIONS));
                    });
                    ui.horizontal(|ui| {
                        let auto_tune = ui.add_enabled(
                            !self.config.optimize_upload,
                            egui::Checkbox::new(&mut self.config.auto_tune_connections, tr(lang, "auto_tune_connections"))
                        );
                        if auto_tune.changed() {
                            let _ = self.config.save();
                        }
                    });
                    let note1 = tr(lang, "note_conn_1");
                    ui.label(self.note_text(&note1));
                    let note2 = tr(lang, "note_conn_2");
                    ui.label(self.note_text(&note2));
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "limit_mbps"));
                        let mut limit = self.config.bandwidth_limit_mbps;
                        let response = ui.add(
                            egui::DragValue::new(&mut limit)
                                .clamp_range(0.0..=1000.0)
                                .speed(1.0)
                        );
                        if response.changed() {
                            self.config.bandwidth_limit_mbps = limit.max(0.0);
                            let _ = self.config.save();
                        }
                        if self.config.bandwidth_limit_mbps <= 0.0 {
                            ui.label("(unlimited)");
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr(lang, "resume"));
                        egui::ComboBox::from_id_source("resume_mode_combo")
                            .selected_text(match self.config.resume_mode.as_str() {
                                "size" => tr(lang, "resume_fast"),
                                "size_mtime" => tr(lang, "resume_medium"),
                                "sha256" => tr(lang, "resume_slow"),
                                _ => tr(lang, "off"),
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(self.config.resume_mode == "none", tr(lang, "off")).clicked() {
                                    self.config.resume_mode = "none".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.resume_mode == "size", tr(lang, "resume_fast")).clicked() {
                                    self.config.resume_mode = "size".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.resume_mode == "size_mtime", tr(lang, "resume_medium")).clicked() {
                                    self.config.resume_mode = "size_mtime".to_string();
                                    let _ = self.config.save();
                                }
                                if ui.selectable_label(self.config.resume_mode == "sha256", tr(lang, "resume_slow")).clicked() {
                                    self.config.resume_mode = "sha256".to_string();
                                    let _ = self.config.save();
                                }
                            });
                    });
                    if self.config.resume_mode != "none" {
                        let note_resume = tr(lang, "note_resume");
                        ui.label(self.note_text(&note_resume));
                        let note_resume_change = tr(lang, "note_resume_change");
                        ui.label(self.note_text(&note_resume_change));
                    }
                    ui.horizontal(|ui| {
                        let chmod_toggle = ui.checkbox(&mut self.config.chmod_after_upload, tr(lang, "chmod_after"));
                        if chmod_toggle.changed() {
                            let _ = self.config.save();
                        }
                        let note_chmod = tr(lang, "note_chmod");
                        ui.label(self.note_text(&note_chmod));
                    });

                    if self.is_uploading {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr(lang, "uploading"));
                            if ui.add(egui::Button::new(format!("❌ {}", tr(lang, "stop"))).min_size([100.0, 32.0].into())).clicked() {
                                self.upload_cancellation_token.store(true, Ordering::Relaxed);
                                self.log("Stopping...");
                            }
                        });
                    } else if self.is_downloading {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr(lang, "downloading"));
                        });
                    } else {
                        ui.add_space(15.0);
                        let enabled = !self.game_path.is_empty() && self.selected_storage.is_some() && connected && !self.is_downloading;
                        let has_pending = self.queue_data.items.iter().any(|i| i.status == QueueStatus::Pending);
                        ui.horizontal(|ui| {
                            if ui.add_enabled(enabled, egui::Button::new(format!("🚀 {}", tr(lang, "upload_current"))).min_size([170.0, 40.0].into())).clicked() {
                                self.check_exists_and_upload();
                            }
                            if ui.add_enabled(has_pending && connected, egui::Button::new(format!("📦 {}", tr(lang, "upload_queue_btn"))).min_size([170.0, 40.0].into())).clicked() {
                                self.process_next_queue_item();
                            }
                        });
                    }
                });
            }
            });
        });
    }
}

async fn fetch_latest_release(include_prerelease: bool) -> anyhow::Result<ReleaseInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/repos/phantomptr/ps5upload/releases")
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;

    let releases = response.json::<Vec<ReleaseInfo>>().await?;
    if include_prerelease {
        releases.into_iter().next().ok_or_else(|| anyhow::anyhow!("No releases found"))
    } else {
        releases
            .into_iter()
            .find(|r| !r.prerelease)
            .ok_or_else(|| anyhow::anyhow!("No stable releases found"))
    }
}

async fn fetch_release_by_tag(tag: &str) -> anyhow::Result<ReleaseInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("https://api.github.com/repos/phantomptr/ps5upload/releases/tags/{}", tag))
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

fn extract_zip(zip_path: &std::path::Path, dest_dir: &std::path::Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dest_dir)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest_dir.join(file.mangled_name());
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut outfile = std::fs::File::create(&outpath)?;
        std::io::copy(&mut file, &mut outfile)?;
    }
    Ok(())
}

fn find_app_bundle(root: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in walkdir::WalkDir::new(root).max_depth(3).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_dir() {
            if entry.path().extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("app")).unwrap_or(false) {
                return Some(entry.path().to_path_buf());
            }
        }
    }
    None
}

fn find_file_by_name(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            if entry.file_name() == name {
                return Some(entry.path().to_path_buf());
            }
        }
    }
    None
}

fn build_pending_update(extract_dir: &std::path::Path) -> anyhow::Result<PendingUpdate> {
    let current_exe = std::env::current_exe()?;
    let mut bundle_root: Option<std::path::PathBuf> = None;
    let mut cursor = current_exe.as_path();
    while let Some(parent) = cursor.parent() {
        if cursor.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("app")).unwrap_or(false) {
            bundle_root = Some(cursor.to_path_buf());
            break;
        }
        cursor = parent;
    }

    if let Some(bundle_root) = bundle_root {
        let replacement = find_app_bundle(extract_dir)
            .ok_or_else(|| anyhow::anyhow!("Update bundle not found"))?;
        let restart_path = current_exe.clone();
        return Ok(PendingUpdate {
            target_path: bundle_root,
            replacement_path: replacement,
            restart_path,
            is_dir: true,
        });
    }

    let exe_name = current_exe.file_name().and_then(|s| s.to_str()).ok_or_else(|| anyhow::anyhow!("Invalid executable name"))?;
    let replacement = find_file_by_name(extract_dir, exe_name)
        .ok_or_else(|| anyhow::anyhow!("Update binary not found"))?;
    Ok(PendingUpdate {
        target_path: current_exe.clone(),
        replacement_path: replacement,
        restart_path: current_exe,
        is_dir: false,
    })
}

fn spawn_update_helper(pending: &PendingUpdate) -> anyhow::Result<()> {
    let helper_dir = std::env::temp_dir();
    let pid = std::process::id();
    let target = pending.target_path.to_string_lossy().to_string();
    let replacement = pending.replacement_path.to_string_lossy().to_string();
    let restart = pending.restart_path.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        let script_path = helper_dir.join("ps5upload_update.cmd");
        let script = format!(
            "@echo off\r\n:wait\r\ntasklist /FI \"PID eq {}\" | find \"{}\" >nul\r\nif not errorlevel 1 (timeout /t 1 >nul & goto wait)\r\n",
            pid, pid
        );
        let script = if pending.is_dir {
            format!(
                "{}rmdir /S /Q \"{}\"\r\nmove /Y \"{}\" \"{}\"\r\nstart \"\" \"{}\"\r\n",
                script, target, replacement, target, restart
            )
        } else {
            format!(
                "{}del /F /Q \"{}\"\r\nmove /Y \"{}\" \"{}\"\r\nstart \"\" \"{}\"\r\n",
                script, target, replacement, target, restart
            )
        };
        std::fs::write(&script_path, script)?;
        std::process::Command::new("cmd")
            .args(["/C", script_path.to_string_lossy().as_ref()])
            .spawn()?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        let script_path = helper_dir.join("ps5upload_update.sh");
        let script = format!(
            "#!/bin/sh\nwhile kill -0 {} 2>/dev/null; do sleep 0.2; done\n",
            pid
        );
        let script = if pending.is_dir {
            format!(
                "{}rm -rf \"{}\"\nmv \"{}\" \"{}\"\n\"{}\" &\n",
                script, target, replacement, target, restart
            )
        } else {
            format!(
                "{}rm -f \"{}\"\nmv \"{}\" \"{}\"\nchmod +x \"{}\"\n\"{}\" &\n",
                script, target, replacement, target, restart, restart
            )
        };
        std::fs::write(&script_path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms)?;
        }
        std::process::Command::new("sh")
            .arg(script_path)
            .spawn()?;
        return Ok(());
    }
}

fn normalize_version(version: &str) -> String {
    version.trim_start_matches('v').trim().to_string()
}

fn chat_room_id_for_key(key_hex: &str) -> String {
    let bytes = hex::decode(key_hex).unwrap_or_else(|_| key_hex.as_bytes().to_vec());
    let digest = Sha256::digest(&bytes);
    let full = hex::encode(digest);
    full.chars().take(8).collect()
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

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Load Simplified Chinese font
    fonts.font_data.insert(
        "noto_sans_sc".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/NotoSansSC-Regular.otf")),
    );

    // Load Traditional Chinese font
    fonts.font_data.insert(
        "noto_sans_tc".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/NotoSansTC-Regular.otf")),
    );

    // Load Arabic font
    fonts.font_data.insert(
        "noto_sans_arabic".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/NotoSansArabic-Regular.ttf")),
    );

    // Add fonts as fallbacks for Proportional family (after default fonts)
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .extend([
            "noto_sans_sc".to_owned(),
            "noto_sans_tc".to_owned(),
            "noto_sans_arabic".to_owned(),
        ]);

    ctx.set_fonts(fonts);
}

fn setup_custom_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_rounding = 8.0.into();
    visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 215);
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(245, 245, 245));
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
    visuals.selection.bg_fill = egui::Color32::from_rgb(0, 92, 171);
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 255, 255));
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

fn load_logo_rgba(max_dim: Option<u32>) -> Option<(Vec<u8>, u32, u32)> {
    let mut image = image::load_from_memory(LOGO_BYTES).ok()?;
    if let Some(max_dim) = max_dim {
        let (w, h) = image.dimensions();
        let max_side = w.max(h);
        if max_side > max_dim {
            let scale = max_dim as f32 / max_side as f32;
            let nw = (w as f32 * scale).round().max(1.0) as u32;
            let nh = (h as f32 * scale).round().max(1.0) as u32;
            let resized = image::imageops::resize(&image, nw, nh, image::imageops::FilterType::Lanczos3);
            image = image::DynamicImage::ImageRgba8(resized);
        }
    }
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some((rgba.into_raw(), width, height))
}

fn load_logo_image() -> Option<egui::ColorImage> {
    let (pixels, width, height) = load_logo_rgba(None)?;
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &pixels,
    ))
}

fn main() -> eframe::Result<()> {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1932.0, 1242.0])
                .with_min_inner_size([1656.0, 1104.0])
                .with_icon(std::sync::Arc::new(build_icon())),
            ..Default::default()
        };
    eframe::run_native("PS5 Upload", options, Box::new(|cc| Box::new(Ps5UploadApp::new(cc))))
}

fn build_icon() -> egui::IconData {
    if let Some((pixels, width, height)) = load_logo_rgba(Some(64)) {
        return egui::IconData {
            rgba: pixels,
            width,
            height,
        };
    }

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

fn sha256_file(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}


fn sample_workload(path: &str, cancel: &Arc<AtomicBool>) -> Option<(usize, u64)> {
    let path = Path::new(path);
    if path.is_file() {
        let size = path.metadata().ok().map(|m| m.len()).unwrap_or(0);
        return Some((1, size));
    }
    let mut count = 0usize;
    let mut total = 0u64;
    let start = std::time::Instant::now();
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if start.elapsed() > std::time::Duration::from_millis(300) {
            break;
        }
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
        count += 1;
        if count >= 2000 {
            break;
        }
    }
    if count == 0 { None } else { Some((count, total)) }
}

fn sample_bytes_from_path(path: &str, cancel: &Arc<AtomicBool>) -> Option<Vec<u8>> {
    let path = Path::new(path);
    let max_total = 512 * 1024;
    let per_file = 64 * 1024;
    let mut out = Vec::with_capacity(max_total.min(64 * 1024));
    let mut total = 0usize;

    if path.is_file() {
        if !cancel.load(Ordering::Relaxed) && total < max_total {
            if let Ok(mut file) = std::fs::File::open(path) {
                let remaining = max_total - total;
                let to_read = per_file.min(remaining);
                let mut buf = vec![0u8; to_read];
                if let Ok(n) = file.read(&mut buf) {
                    if n > 0 {
                        out.extend_from_slice(&buf[..n]);
                    }
                }
            }
        }
        return if out.is_empty() { None } else { Some(out) };
    } else {
        let start = std::time::Instant::now();
        let mut files_seen = 0usize;
        for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if start.elapsed() > std::time::Duration::from_millis(400) {
                break;
            }
            let entry_path = entry.path();
            if !entry_path.is_file() {
                continue;
            }
            if total < max_total {
                if let Ok(mut file) = std::fs::File::open(entry_path) {
                    let remaining = max_total - total;
                    let to_read = per_file.min(remaining);
                    let mut buf = vec![0u8; to_read];
                    if let Ok(n) = file.read(&mut buf) {
                        if n > 0 {
                            out.extend_from_slice(&buf[..n]);
                            total += n;
                        }
                    }
                }
            }
            files_seen += 1;
            if total >= max_total || files_seen >= 200 {
                break;
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

fn sample_bytes_from_files(files: &[FileEntry], cancel: &Arc<AtomicBool>) -> Option<Vec<u8>> {
    let max_total = 512 * 1024;
    let per_file = 64 * 1024;
    let mut out = Vec::with_capacity(max_total.min(64 * 1024));
    let mut total = 0usize;

    for entry in files.iter() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if total >= max_total {
            break;
        }
        if let Ok(mut file) = std::fs::File::open(&entry.abs_path) {
            let remaining = max_total - total;
            let to_read = per_file.min(remaining);
            let mut buf = vec![0u8; to_read];
            if let Ok(n) = file.read(&mut buf) {
                if n > 0 {
                    out.extend_from_slice(&buf[..n]);
                    total += n;
                }
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

fn optimize_upload_settings(path: &str, cancel: &Arc<AtomicBool>, current_connections: usize) -> UploadOptimization {
    let mut opt = UploadOptimization {
        compression: None,
        connections: None,
        sample_files: None,
        sample_bytes: None,
    };

    if let Some((count, bytes)) = sample_workload(path, cancel) {
        opt.sample_files = Some(count);
        opt.sample_bytes = Some(bytes);
        opt.connections = Some(recommend_connections(current_connections, count, bytes));
    }

    if let Some(sample) = sample_bytes_from_path(path, cancel) {
        opt.compression = Some(choose_best_compression(&sample));
    }

    opt
}

fn choose_best_compression(sample: &[u8]) -> CompressionMode {
    let raw_size = sample.len();
    if raw_size == 0 {
        return CompressionMode::None;
    }

    let lz4 = compress_prepend_size(sample);
    let lz4_size = lz4.len();
    if lz4_size < raw_size {
        CompressionMode::Lz4
    } else {
        CompressionMode::None
    }
}

fn compression_label(mode: CompressionMode) -> &'static str {
    match mode {
        CompressionMode::None => "None",
        CompressionMode::Lz4 => "LZ4",
        CompressionMode::Zstd => "Zstd",
        CompressionMode::Lzma => "LZMA",
    }
}

fn compression_mode_to_setting(mode: CompressionMode) -> &'static str {
    match mode {
        CompressionMode::None => "none",
        CompressionMode::Lz4 => "lz4",
        CompressionMode::Zstd => "zstd",
        CompressionMode::Lzma => "lzma",
    }
}

fn recommend_connections(current: usize, sample_count: usize, sample_bytes: u64) -> usize {
    if sample_count < 200 || current <= 1 {
        return current;
    }
    let avg = sample_bytes / sample_count as u64;
    if avg < 8 * 1024 {
        1
    } else if avg < 64 * 1024 {
        current.min(2)
    } else if avg < 512 * 1024 {
        current.min(4)
    } else {
        current
    }
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

fn read_upload_response(
    stream: &mut std::net::TcpStream,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<String> {
    use std::io::Read;
    use std::io::ErrorKind;
    let start = std::time::Instant::now();
    let mut buffer = [0u8; 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Upload cancelled"));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Err(anyhow::anyhow!("Socket closed during response read")),
            Ok(n) => return Ok(String::from_utf8_lossy(&buffer[..n]).trim().to_string()),
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
                    if start.elapsed() >= std::time::Duration::from_secs(10) {
                        return Err(anyhow::anyhow!("Timed out waiting for server response"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                _ => return Err(err.into()),
            },
        }
    }
}

fn partition_files_by_size(mut files: Vec<FileEntry>, connections: usize) -> Vec<Vec<FileEntry>> {
    let connections = connections.max(1);
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
