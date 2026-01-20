/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Application state management with grouped sub-states.

use crate::chat::ChatMessage;
use crate::config::AppConfig;
use crate::history::{HistoryData, TransferRecord};
use crate::i18n::Language;
use crate::message::{GameMeta, PendingUpdate, ReleaseInfo};
use crate::profiles::{Profile, ProfilesData};
use crate::protocol::{DirEntry, StorageLocation};
use crate::queue::{QueueData, QueueItem};
use crate::ui::DialogState;
use eframe::egui;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use tokio::runtime::Runtime;

/// Profile snapshot for auto-save detection
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSnapshot {
    pub address: String,
    pub storage: String,
    pub preset_index: usize,
    pub custom_preset_path: String,
    pub connections: usize,
    pub use_temp: bool,
    pub auto_tune_connections: bool,
}

/// Connection state (PS5 address, connectivity)
#[derive(Default)]
pub struct ConnectionState {
    pub ip: String,
    pub is_connected: bool,
    pub is_connecting: bool,
    pub storage_locations: Vec<StorageLocation>,
    pub selected_storage: Option<String>,
    pub last_auto_connect_attempt: Option<std::time::Instant>,
}

/// Payload state (status, sending)
pub struct PayloadState {
    pub path: String,
    pub status: String,
    pub version: Option<String>,
    pub is_sending: bool,
    pub last_check: Option<std::time::Instant>,
}

impl Default for PayloadState {
    fn default() -> Self {
        Self {
            path: String::new(),
            status: "Unknown (not checked)".to_string(),
            version: None,
            is_sending: false,
            last_check: None,
        }
    }
}

/// Transfer state (source, destination, settings)
#[derive(Default)]
pub struct TransferState {
    pub game_path: String,
    pub selected_preset: usize,
    pub custom_preset_path: String,
    pub custom_subfolder: String,
    pub forced_dest_path: Option<String>,
    pub is_uploading: bool,
    pub is_scanning: bool,
    pub auto_resume_on_exists: bool,
    pub game_meta: Option<GameMeta>,
    pub game_cover_texture: Option<egui::TextureHandle>,
    pub calculating_size: bool,
    pub calculated_size: Option<u64>,
    pub upload_cancellation_token: Arc<AtomicBool>,
    pub upload_run_id: u64,
    pub upload_start_time: Option<std::time::Instant>,
    pub upload_source_path: String,
    pub upload_dest_path: String,
}

/// Progress state for uploads
#[derive(Default)]
pub struct ProgressState {
    pub sent: u64,
    pub total: u64,
    pub speed_bps: f64,
    pub eta_secs: Option<f64>,
    pub files: i32,
    pub current_file: String,
    pub phase: String,
    pub scanning_files_found: usize,
    pub scanning_total_size: u64,
}

impl ProgressState {
    pub fn reset(&mut self) {
        self.sent = 0;
        self.total = 0;
        self.speed_bps = 0.0;
        self.eta_secs = None;
        self.files = 0;
        self.current_file.clear();
        self.phase.clear();
        self.scanning_files_found = 0;
        self.scanning_total_size = 0;
    }
}

/// Download state
#[derive(Default)]
pub struct DownloadState {
    pub is_downloading: bool,
    pub cancellation_token: Arc<AtomicBool>,
    pub progress_sent: u64,
    pub progress_total: u64,
    pub speed_bps: f64,
    pub eta_secs: Option<f64>,
    pub current_file: String,
    pub start_time: Option<std::time::Instant>,
}

impl DownloadState {
    pub fn reset(&mut self) {
        self.is_downloading = false;
        self.progress_sent = 0;
        self.progress_total = 0;
        self.speed_bps = 0.0;
        self.eta_secs = None;
        self.current_file.clear();
        self.start_time = None;
    }
}

/// Manage tab state (file browser, operations)
#[derive(Default)]
pub struct ManageState {
    pub left_path: String,
    pub right_path: String,
    pub left_entries: Vec<DirEntry>,
    pub right_entries: Vec<DirEntry>,
    pub left_selected: Option<usize>,
    pub right_selected: Option<usize>,
    pub left_status: String,
    pub right_status: String,
    pub new_name: String,
    pub busy: bool,
    pub meta: Option<GameMeta>,
    pub cover_texture: Option<egui::TextureHandle>,
    pub meta_path: Option<String>,
    // Move/Copy operation state
    pub is_moving: bool,
    pub move_cancellation_token: Arc<AtomicBool>,
    pub op_cancellation_token: Arc<AtomicBool>,
    pub progress_active: bool,
    pub progress_op: String,
    pub progress_sent: u64,
    pub progress_total: u64,
    pub progress_start_time: Option<std::time::Instant>,
}

impl ManageState {
    pub fn new() -> Self {
        Self {
            left_path: "/data".to_string(),
            right_path: "/data".to_string(),
            left_status: "Not connected".to_string(),
            right_status: "Not connected".to_string(),
            move_cancellation_token: Arc::new(AtomicBool::new(false)),
            op_cancellation_token: Arc::new(AtomicBool::new(false)),
            ..Default::default()
        }
    }

    pub fn reset_progress(&mut self) {
        self.progress_active = false;
        self.progress_sent = 0;
        self.progress_total = 0;
        self.progress_op.clear();
        self.progress_start_time = None;
    }
}

/// Chat state
#[derive(Default)]
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub status: String,
    pub tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    pub sent_count: u64,
    pub received_count: u64,
    pub ack_count: u64,
    pub reject_count: u64,
    pub room_id: String,
}

/// Update state (app updates)
pub struct UpdateState {
    pub info: Option<ReleaseInfo>,
    pub status: String,
    pub available: bool,
    pub check_running: bool,
    pub download_status: String,
    pub pending: Option<PendingUpdate>,
    pub next_check: Option<std::time::Instant>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            info: None,
            status: "Checking for updates...".to_string(),
            available: false,
            check_running: false,
            download_status: String::new(),
            pending: None,
            next_check: None,
        }
    }
}

/// Profile management state
#[derive(Default)]
pub struct ProfileState {
    pub data: ProfilesData,
    pub current: Option<String>,
    pub editing: Option<Profile>,
    pub dirty: bool,
    pub last_snapshot: Option<ProfileSnapshot>,
    pub last_change: Option<std::time::Instant>,
}

/// Queue state
#[derive(Default)]
pub struct QueueState {
    pub data: QueueData,
    pub current_item_id: Option<u64>,
}

/// Main application state grouping all sub-states
pub struct AppState {
    // Grouped states
    pub connection: ConnectionState,
    pub payload: PayloadState,
    pub transfer: TransferState,
    pub progress: ProgressState,
    pub download: DownloadState,
    pub manage: ManageState,
    pub chat: ChatState,
    pub updates: UpdateState,
    pub dialogs: DialogState,
    pub profile: ProfileState,
    pub queue: QueueState,

    // Shared/persistent
    pub config: AppConfig,
    pub history_data: HistoryData,

    // UI state
    pub main_tab: usize,
    pub log_tab: usize,
    pub language: Language,
    pub theme_dark: bool,
    pub status: String,
    pub client_logs: String,
    pub payload_logs: String,
    pub logo_texture: Option<egui::TextureHandle>,

    // Runtime
    pub rx: Receiver<crate::message::AppMessage>,
    pub tx: Sender<crate::message::AppMessage>,
    pub rt: Arc<Runtime>,
}

impl AppState {
    /// Create a new AppState with default values
    pub fn new(
        config: AppConfig,
        history_data: HistoryData,
        profiles_data: ProfilesData,
        queue_data: QueueData,
        tx: Sender<crate::message::AppMessage>,
        rx: Receiver<crate::message::AppMessage>,
        rt: Runtime,
    ) -> Self {
        let theme_dark = config.theme != "light";
        let language = Language::from_code(&config.language);

        let mut connection = ConnectionState::default();
        connection.ip = config.address.clone();
        connection.selected_storage = Some(config.storage.clone());

        let mut profile = ProfileState::default();
        profile.data = profiles_data;

        let mut queue = QueueState::default();
        queue.data = queue_data;

        Self {
            connection,
            payload: PayloadState::default(),
            transfer: TransferState {
                upload_cancellation_token: Arc::new(AtomicBool::new(false)),
                ..Default::default()
            },
            progress: ProgressState::default(),
            download: DownloadState {
                cancellation_token: Arc::new(AtomicBool::new(false)),
                ..Default::default()
            },
            manage: ManageState::new(),
            chat: ChatState {
                status: crate::i18n::tr(language, "chat_connecting"),
                ..Default::default()
            },
            updates: UpdateState::default(),
            dialogs: DialogState::new(),
            profile,
            queue,
            config,
            history_data,
            main_tab: 0,
            log_tab: 0,
            language,
            theme_dark,
            status: "Ready".to_string(),
            client_logs: String::new(),
            payload_logs: String::new(),
            logo_texture: None,
            rx,
            tx,
            rt: Arc::new(rt),
        }
    }

    /// Add a log message to client logs
    pub fn log(&mut self, message: &str) {
        use std::fmt::Write;
        let ts = chrono::Local::now().format("%H:%M:%S");
        let _ = writeln!(self.client_logs, "[{}] {}", ts, message);
        self.trim_logs();
    }

    /// Add a log message to payload logs
    pub fn payload_log(&mut self, message: &str) {
        use std::fmt::Write;
        let ts = chrono::Local::now().format("%H:%M:%S");
        let _ = writeln!(self.payload_logs, "[{}] {}", ts, message);
        self.trim_logs();
    }

    /// Trim logs if they exceed the maximum size
    fn trim_logs(&mut self) {
        const MAX_LOG_BYTES: usize = 512 * 1024;
        if self.client_logs.len() > MAX_LOG_BYTES {
            let trim_to = MAX_LOG_BYTES * 2 / 3;
            if let Some(pos) = self.client_logs[..trim_to].rfind('\n') {
                self.client_logs = self.client_logs[pos + 1..].to_string();
            }
        }
        if self.payload_logs.len() > MAX_LOG_BYTES {
            let trim_to = MAX_LOG_BYTES * 2 / 3;
            if let Some(pos) = self.payload_logs[..trim_to].rfind('\n') {
                self.payload_logs = self.payload_logs[pos + 1..].to_string();
            }
        }
    }

    /// Check if any operation is currently busy
    pub fn is_busy(&self) -> bool {
        self.transfer.is_uploading
            || self.transfer.is_scanning
            || self.download.is_downloading
            || self.manage.busy
            || self.manage.progress_active
            || self.payload.is_sending
            || self.connection.is_connecting
    }

    /// Get current profile snapshot for auto-save detection
    pub fn current_profile_snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            address: self.connection.ip.clone(),
            storage: self
                .connection
                .selected_storage
                .clone()
                .unwrap_or_else(|| "/data".to_string()),
            preset_index: self.transfer.selected_preset,
            custom_preset_path: self.transfer.custom_preset_path.clone(),
            connections: self.config.connections,
            use_temp: self.config.use_temp,
            auto_tune_connections: self.config.auto_tune_connections,
        }
    }

    /// Check if profile matches a snapshot
    pub fn profile_matches_snapshot(profile: &Profile, snap: &ProfileSnapshot) -> bool {
        profile.address == snap.address
            && profile.storage == snap.storage
            && profile.preset_index == snap.preset_index
            && profile.custom_preset_path == snap.custom_preset_path
            && profile.connections == snap.connections
            && profile.use_temp == snap.use_temp
            && profile.auto_tune_connections == snap.auto_tune_connections
    }

    /// Push a chat message to the chat history
    pub fn push_chat_message(&mut self, msg: ChatMessage) {
        const CHAT_MAX_MESSAGES: usize = 500;
        if self.chat.messages.len() >= CHAT_MAX_MESSAGES {
            self.chat.messages.remove(0);
        }
        self.chat.messages.push(msg);
    }

    /// Add a history record
    pub fn add_history_record(&mut self, record: TransferRecord) {
        let _ = crate::history::add_record(&mut self.history_data, record);
    }
}
