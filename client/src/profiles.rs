use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub address: String,
    pub storage: String,
    pub preset_index: usize,
    pub custom_preset_path: String,
    pub connections: usize,
    pub use_temp: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Default".to_string(),
            address: "192.168.0.100".to_string(),
            storage: "/data".to_string(),
            preset_index: 0,
            custom_preset_path: String::new(),
            connections: 5,
            use_temp: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilesData {
    pub profiles: Vec<Profile>,
    pub default_profile: Option<String>,
}

const PROFILES_FILE: &str = "ps5upload_profiles.json";

pub fn load_profiles() -> ProfilesData {
    let path = Path::new(PROFILES_FILE);
    if !path.exists() {
        return ProfilesData::default();
    }

    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ProfilesData::default(),
    }
}

pub fn save_profiles(data: &ProfilesData) {
    if let Ok(content) = serde_json::to_string_pretty(data) {
        let _ = fs::write(PROFILES_FILE, content);
    }
}
