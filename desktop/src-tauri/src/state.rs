use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use ps5upload_core::message::PendingUpdate;
use tokio::sync::mpsc;

pub struct AppState {
    pub transfer_cancel: Arc<AtomicBool>,
    pub transfer_active: Arc<AtomicBool>,
    pub transfer_run_id: AtomicU64,
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
            manage_cancel: Arc::new(AtomicBool::new(false)),
            manage_active: Arc::new(AtomicBool::new(false)),
            chat_sender: Arc::new(Mutex::new(None)),
            pending_update: Arc::new(Mutex::new(None)),
        }
    }
}
