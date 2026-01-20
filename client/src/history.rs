use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    pub timestamp: i64,
    pub source_path: String,
    pub dest_path: String,
    pub file_count: i32,
    pub total_bytes: u64,
    pub duration_secs: f64,
    pub speed_bps: f64,
    pub success: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub via_queue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryData {
    pub records: Vec<TransferRecord>,
}

const HISTORY_FILE: &str = "ps5upload_history.json";
const MAX_HISTORY_ITEMS: usize = 100;

pub fn load_history_from(path: &Path) -> HistoryData {
    if !path.exists() {
        return HistoryData::default();
    }

    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HistoryData::default(),
    }
}

pub fn load_history() -> HistoryData {
    load_history_from(Path::new(HISTORY_FILE))
}

pub fn save_history_to(data: &HistoryData, path: &Path) -> Result<(), std::io::Error> {
    // Limit to MAX_HISTORY_ITEMS
    let mut limited = data.clone();
    if limited.records.len() > MAX_HISTORY_ITEMS {
        limited.records = limited
            .records
            .split_off(limited.records.len() - MAX_HISTORY_ITEMS);
    }

    let content = serde_json::to_string_pretty(&limited).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, content)
}

pub fn save_history(data: &HistoryData) {
    let _ = save_history_to(data, Path::new(HISTORY_FILE));
}

pub fn add_record(data: &mut HistoryData, record: TransferRecord) {
    data.records.push(record);
    save_history(data);
}

pub fn clear_history_to(data: &mut HistoryData, path: &Path) -> Result<(), std::io::Error> {
    data.records.clear();
    save_history_to(data, path)
}

pub fn clear_history(data: &mut HistoryData) {
    let _ = clear_history_to(data, Path::new(HISTORY_FILE));
}
