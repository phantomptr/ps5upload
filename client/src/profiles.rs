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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfilesData {
    pub profiles: Vec<Profile>,
    pub default_profile: Option<String>,
}

const PROFILES_FILE: &str = "ps5upload_profiles.ini";
const LEGACY_PROFILES_FILE: &str = "ps5upload_profiles.json";

enum ProfileSection {
    Global,
    Profiles,
    Profile(String),
}

fn parse_section(line: &str) -> Option<ProfileSection> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
    if inner.eq_ignore_ascii_case("profiles") {
        return Some(ProfileSection::Profiles);
    }
    let inner_lower = inner.to_lowercase();
    if inner_lower.starts_with("profile ") {
        let name = inner[8..].trim();
        if name.starts_with('"') && name.ends_with('"') && name.len() >= 2 {
            return Some(ProfileSection::Profile(name[1..name.len() - 1].to_string()));
        }
        return Some(ProfileSection::Profile(name.to_string()));
    }
    if inner_lower.starts_with("profile:") {
        let name = inner[8..].trim();
        return Some(ProfileSection::Profile(name.to_string()));
    }
    None
}

fn get_profile_mut<'a>(data: &'a mut ProfilesData, name: &str) -> &'a mut Profile {
    if let Some((idx, _)) = data.profiles.iter().enumerate().find(|(_, p)| p.name == name) {
        return &mut data.profiles[idx];
    }
    data.profiles.push(Profile {
        name: name.to_string(),
        ..Profile::default()
    });
    let len = data.profiles.len();
    &mut data.profiles[len - 1]
}

pub fn load_profiles() -> ProfilesData {
    let path = Path::new(PROFILES_FILE);
    if !path.exists() {
        let legacy_path = Path::new(LEGACY_PROFILES_FILE);
        if legacy_path.exists() {
            if let Ok(content) = fs::read_to_string(legacy_path) {
                if let Ok(data) = serde_json::from_str::<ProfilesData>(&content) {
                    save_profiles(&data);
                    return data;
                }
            }
        }
        return ProfilesData::default();
    }

    match fs::read_to_string(path) {
        Ok(content) => {
            let mut data = ProfilesData::default();
            let mut section = ProfileSection::Global;
            for raw_line in content.lines() {
                let line = raw_line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                    continue;
                }
                if let Some(next_section) = parse_section(line) {
                    section = next_section;
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else { continue; };
                let key = key.trim();
                let value = value.trim();
                match &section {
                    ProfileSection::Profiles => {
                        if key == "default" && !value.is_empty() {
                            data.default_profile = Some(value.to_string());
                        }
                    }
                    ProfileSection::Profile(name) => {
                        let profile = get_profile_mut(&mut data, name);
                        match key {
                            "address" => profile.address = value.to_string(),
                            "storage" => profile.storage = value.to_string(),
                            "preset_index" => {
                                if let Ok(parsed) = value.parse::<usize>() {
                                    profile.preset_index = parsed;
                                }
                            }
                            "custom_preset_path" => profile.custom_preset_path = value.to_string(),
                            "connections" => {
                                if let Ok(parsed) = value.parse::<usize>() {
                                    profile.connections = parsed.max(1);
                                }
                            }
                            "use_temp" => {
                                profile.use_temp = matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
                            }
                            _ => {}
                        }
                    }
                    ProfileSection::Global => {}
                }
            }
            data
        }
        Err(_) => ProfilesData::default(),
    }
}

pub fn save_profiles(data: &ProfilesData) {
    let mut content = String::new();
    content.push_str("[profiles]\n");
    if let Some(default_name) = &data.default_profile {
        content.push_str(&format!("default={}\n", default_name));
    }
    content.push('\n');

    for profile in &data.profiles {
        content.push_str(&format!("[profile \"{}\"]\n", profile.name));
        content.push_str(&format!("address={}\n", profile.address));
        content.push_str(&format!("storage={}\n", profile.storage));
        content.push_str(&format!("preset_index={}\n", profile.preset_index));
        content.push_str(&format!("custom_preset_path={}\n", profile.custom_preset_path));
        content.push_str(&format!("connections={}\n", profile.connections));
        content.push_str(&format!("use_temp={}\n", if profile.use_temp { "true" } else { "false" }));
        content.push('\n');
    }

    let _ = fs::write(PROFILES_FILE, content);
}
