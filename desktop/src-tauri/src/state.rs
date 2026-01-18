use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

pub struct AppState {
    pub transfer_cancel: Arc<AtomicBool>,
    pub transfer_active: Arc<AtomicBool>,
    pub transfer_run_id: AtomicU64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            transfer_cancel: Arc::new(AtomicBool::new(false)),
            transfer_active: Arc::new(AtomicBool::new(false)),
            transfer_run_id: AtomicU64::new(0),
        }
    }
}
