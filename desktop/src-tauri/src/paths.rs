use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[allow(dead_code)]
pub struct AppPaths {
    pub config: PathBuf,
    pub history: PathBuf,
    pub queue: PathBuf,
    pub profiles: PathBuf,
    pub profiles_json: PathBuf,
    pub logs_dir: PathBuf,
}

fn resolve_base_dir(app: &AppHandle) -> PathBuf {
    let exe_dir = app
        .path()
        .executable_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    exe_dir.join(".ps5upload")
}

pub fn resolve_paths(app: &AppHandle) -> AppPaths {
    let base_dir = resolve_base_dir(app);
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
