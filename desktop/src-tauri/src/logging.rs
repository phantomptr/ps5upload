use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use crate::paths::resolve_paths;

fn sanitize_tag(tag: &str) -> String {
    tag.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

pub fn write_log_line(app: &AppHandle, tag: &str, message: &str) {
    let paths = resolve_paths(app);
    let safe_tag = sanitize_tag(tag);
    let file_name = if safe_tag.is_empty() { "desktop" } else { safe_tag.as_str() };
    let path = paths.logs_dir.join(format!("{file_name}.log"));
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(_) => return,
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = writeln!(file, "[{}] {}", ts, message);
}
