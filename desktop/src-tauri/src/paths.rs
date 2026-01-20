use std::path::PathBuf;
use tauri::AppHandle;

#[allow(dead_code)]
pub struct AppPaths {
    pub config: PathBuf,
    pub history: PathBuf,
    pub queue: PathBuf,
    pub profiles: PathBuf,
    pub profiles_json: PathBuf,
    pub logs_dir: PathBuf,
}

fn resolve_base_dir() -> PathBuf {
    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home_dir.join(".ps5upload")
}

pub fn resolve_paths(_app: &AppHandle) -> AppPaths {
    let base_dir = resolve_base_dir();
    let _ = std::fs::create_dir_all(&base_dir);

    let config = base_dir.join("ps5upload.ini");

    let logs_dir = base_dir.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);

    AppPaths {
        config,
        history: base_dir.join("ps5upload_history.json"),
        queue: base_dir.join("ps5upload_queue.json"),
        profiles: base_dir.join("ps5upload_profiles.ini"),
        profiles_json: base_dir.join("ps5upload_profiles.json"),
        logs_dir,
    }
}
