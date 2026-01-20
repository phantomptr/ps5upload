use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home_dir.join(".ps5upload")
}

pub fn config_path() -> PathBuf {
    data_dir().join("ps5upload.ini")
}

pub fn profiles_path() -> PathBuf {
    data_dir().join("ps5upload_profiles.ini")
}

pub fn profiles_json_path() -> PathBuf {
    data_dir().join("ps5upload_profiles.json")
}

pub fn history_path() -> PathBuf {
    data_dir().join("ps5upload_history.json")
}

pub fn queue_path() -> PathBuf {
    data_dir().join("ps5upload_queue.json")
}
