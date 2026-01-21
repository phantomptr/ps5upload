use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use ps5upload_core::message::PendingUpdate;
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
    pub manage_cancel: Arc<AtomicBool>,
    pub manage_active: Arc<AtomicBool>,
    pub chat_sender: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pub pending_update: Arc<Mutex<Option<PendingUpdate>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            transfer_cancel: Arc::new(AtomicBool::new(false)),
            transfer_active: Arc::new(AtomicBool::new(false)),
            transfer_run_id: AtomicU64::new(0),
            transfer_status: Arc::new(Mutex::new(TransferStatus::default())),
            manage_cancel: Arc::new(AtomicBool::new(false)),
            manage_active: Arc::new(AtomicBool::new(false)),
            chat_sender: Arc::new(Mutex::new(None)),
            pending_update: Arc::new(Mutex::new(None)),
        }
    }
}
