use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use ps5upload_core::config::AppConfig;
use ps5upload_core::message::PendingUpdate;
use ps5upload_core::profiles::ProfilesData;
use ps5upload_core::protocol::{DirEntry, PayloadStatus, StorageLocation};
use ps5upload_core::queue::QueueData;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferStatus {
    pub run_id: u64,
    pub status: String,
    pub sent: u64,
    pub total: u64,
    pub files: i32,
    pub elapsed_secs: f64,
    pub current_file: String,
}

impl Default for TransferStatus {
    fn default() -> Self {
        Self {
            run_id: 0,
            status: "Idle".to_string(),
            sent: 0,
            total: 0,
            files: 0,
            elapsed_secs: 0.0,
            current_file: String::new(),
        }
    }
}

pub struct AppState {
    pub transfer_cancel: Arc<AtomicBool>,
    pub transfer_active: Arc<AtomicBool>,
    pub transfer_run_id: AtomicU64,
    pub transfer_status: Arc<Mutex<TransferStatus>>,
    pub payload_status: Arc<Mutex<PayloadStatusCache>>,
    pub payload_poll_enabled: AtomicBool,
    pub payload_ip: Arc<Mutex<String>>,
    pub payload_auto_reload_enabled: AtomicBool,
    pub payload_auto_reload_mode: Arc<Mutex<String>>,
    pub payload_auto_reload_path: Arc<Mutex<String>>,
    pub payload_auto_reload_inflight: AtomicBool,
    pub connection_status: Arc<Mutex<ConnectionStatusCache>>,
    pub connection_poll_enabled: AtomicBool,
    pub connection_ip: Arc<Mutex<String>>,
    pub connection_auto_enabled: AtomicBool,
    pub config_pending: Arc<Mutex<Option<AppConfig>>>,
    pub config_saver_active: AtomicBool,
    pub profiles_pending: Arc<Mutex<Option<ProfilesData>>>,
    pub profiles_saver_active: AtomicBool,
    pub queue_pending: Arc<Mutex<Option<QueueData>>>,
    pub queue_saver_active: AtomicBool,
    pub save_logs: AtomicBool,
    pub ui_log_enabled: AtomicBool,
    pub manage_cancel: Arc<AtomicBool>,
    pub manage_active: Arc<AtomicBool>,
    pub manage_list_cache: Arc<Mutex<ManageListCache>>,
    pub manage_poll_enabled: AtomicBool,
    pub manage_ip: Arc<Mutex<String>>,
    pub manage_path: Arc<Mutex<String>>,
    pub chat_sender: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pub pending_update: Arc<Mutex<Option<PendingUpdate>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PayloadStatusCache {
    pub status: Option<PayloadStatus>,
    pub error: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatusCache {
    pub is_connected: bool,
    pub status: String,
    pub storage_locations: Vec<StorageLocation>,
}

impl Default for ConnectionStatusCache {
    fn default() -> Self {
        Self {
            is_connected: false,
            status: "Disconnected".to_string(),
            storage_locations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManageListCache {
    pub path: String,
    pub entries: Vec<DirEntry>,
    pub error: Option<String>,
    pub updated_at_ms: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            transfer_cancel: Arc::new(AtomicBool::new(false)),
            transfer_active: Arc::new(AtomicBool::new(false)),
            transfer_run_id: AtomicU64::new(0),
            transfer_status: Arc::new(Mutex::new(TransferStatus::default())),
            payload_status: Arc::new(Mutex::new(PayloadStatusCache::default())),
            payload_poll_enabled: AtomicBool::new(false),
            payload_ip: Arc::new(Mutex::new(String::new())),
            payload_auto_reload_enabled: AtomicBool::new(false),
            payload_auto_reload_mode: Arc::new(Mutex::new("current".to_string())),
            payload_auto_reload_path: Arc::new(Mutex::new(String::new())),
            payload_auto_reload_inflight: AtomicBool::new(false),
            connection_status: Arc::new(Mutex::new(ConnectionStatusCache::default())),
            connection_poll_enabled: AtomicBool::new(false),
            connection_ip: Arc::new(Mutex::new(String::new())),
            connection_auto_enabled: AtomicBool::new(false),
            config_pending: Arc::new(Mutex::new(None)),
            config_saver_active: AtomicBool::new(false),
            profiles_pending: Arc::new(Mutex::new(None)),
            profiles_saver_active: AtomicBool::new(false),
            queue_pending: Arc::new(Mutex::new(None)),
            queue_saver_active: AtomicBool::new(false),
            save_logs: AtomicBool::new(false),
            ui_log_enabled: AtomicBool::new(true),
            manage_cancel: Arc::new(AtomicBool::new(false)),
            manage_active: Arc::new(AtomicBool::new(false)),
            manage_list_cache: Arc::new(Mutex::new(ManageListCache::default())),
            manage_poll_enabled: AtomicBool::new(false),
            manage_ip: Arc::new(Mutex::new(String::new())),
            manage_path: Arc::new(Mutex::new(String::new())),
            chat_sender: Arc::new(Mutex::new(None)),
            pending_update: Arc::new(Mutex::new(None)),
        }
    }
}
