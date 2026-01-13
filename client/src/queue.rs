use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueueStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: u64,
    pub source_path: String,
    pub subfolder_name: String,
    pub preset_index: usize,
    pub custom_preset_path: String,
    pub status: QueueStatus,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueData {
    pub items: Vec<QueueItem>,
    pub next_id: u64,
}

const QUEUE_FILE: &str = "ps5upload_queue.json";

pub fn load_queue() -> QueueData {
    let path = Path::new(QUEUE_FILE);
    if !path.exists() {
        return QueueData::default();
    }

    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => QueueData::default(),
    }
}

pub fn save_queue(data: &QueueData) {
    if let Ok(content) = serde_json::to_string_pretty(data) {
        let _ = fs::write(QUEUE_FILE, content);
    }
}
