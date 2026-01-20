use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub address: String,
    pub storage: String,
    pub connections: usize,
    pub use_temp: bool,
    pub auto_connect: bool,
    pub theme: String,                // "dark" or "light"
    pub compression: String,          // "none", "lz4", "zstd", "lzma", "auto"
    pub bandwidth_limit_mbps: f32,    // 0 = unlimited
    pub update_channel: String,       // "stable" or "all"
    pub download_compression: String, // "none", "lz4", "zstd", "lzma", "auto"
    pub chmod_after_upload: bool,
    pub override_on_conflict: bool,
    pub resume_mode: String, // "none", "size", "size_mtime", "sha256"
    pub language: String,    // "en", "zh-CN", "zh-TW", "fr", "es", "ar"
    pub auto_tune_connections: bool,
    pub auto_check_payload: bool,
    pub payload_auto_reload: bool,
    pub payload_reload_mode: String, // "local", "current", "latest"
    pub payload_local_path: String,
    pub optimize_upload: bool,
    pub chat_display_name: String,
    pub rar_extract_mode: String, // "normal", "safe", "turbo"
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            address: "192.168.0.100".to_string(),
            storage: "/data".to_string(),
            connections: 1, // Default to 1 for maximum reliability
            use_temp: false,
            auto_connect: false,
            theme: "dark".to_string(),
            compression: "none".to_string(),
            bandwidth_limit_mbps: 0.0,
            update_channel: "stable".to_string(),
            download_compression: "none".to_string(),
            chmod_after_upload: false,
            override_on_conflict: false,
            resume_mode: "none".to_string(),
            language: "en".to_string(),
            auto_tune_connections: true,
            auto_check_payload: false,
            payload_auto_reload: false,
            payload_reload_mode: "current".to_string(),
            payload_local_path: String::new(),
            optimize_upload: false,
            chat_display_name: String::new(),
            rar_extract_mode: "turbo".to_string(),
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        Self::load_from(Path::new("ps5upload.ini"))
    }

    pub fn load_from(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }

        let mut content = String::new();
        if let Ok(mut file) = File::open(path) {
            if file.read_to_string(&mut content).is_ok() {
                let mut config = Self::default();
                for line in content.lines() {
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().to_string();
                        match key {
                            "address" => config.address = value,
                            "storage" => config.storage = value,
                            "connections" => {
                                if let Ok(parsed) = value.parse::<usize>() {
                                    config.connections = parsed.max(1);
                                }
                            }
                            "use_temp" => {
                                config.use_temp = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "auto_connect" => {
                                config.auto_connect = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "theme" => {
                                config.theme = if value == "light" {
                                    "light".to_string()
                                } else {
                                    "dark".to_string()
                                };
                            }
                            "compression" => {
                                config.compression = match value.as_str() {
                                    "lz4" | "zstd" | "lzma" | "auto" => value,
                                    _ => "none".to_string(),
                                };
                            }
                            "bandwidth_limit_mbps" => {
                                if let Ok(parsed) = value.parse::<f32>() {
                                    config.bandwidth_limit_mbps = parsed.max(0.0);
                                }
                            }
                            "update_channel" => {
                                config.update_channel = if value == "all" {
                                    "all".to_string()
                                } else {
                                    "stable".to_string()
                                };
                            }
                            "download_compression" => {
                                config.download_compression = match value.as_str() {
                                    "lz4" | "zstd" | "lzma" | "auto" => value,
                                    _ => "none".to_string(),
                                };
                            }
                            "chmod_after_upload" => {
                                config.chmod_after_upload = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "override_on_conflict" => {
                                config.override_on_conflict = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "resume_mode" => {
                                config.resume_mode = match value.as_str() {
                                    "size" => "size".to_string(),
                                    "size_mtime" => "size_mtime".to_string(),
                                    "sha256" => "sha256".to_string(),
                                    _ => "none".to_string(),
                                };
                            }
                            "language" => {
                                config.language = match value.as_str() {
                                    "zh-CN" | "zh-TW" | "fr" | "es" | "ar" => value,
                                    _ => "en".to_string(),
                                };
                            }
                            "auto_tune_connections" => {
                                config.auto_tune_connections = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "auto_check_payload" => {
                                config.auto_check_payload = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "payload_auto_reload" => {
                                config.payload_auto_reload = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "payload_reload_mode" => {
                                config.payload_reload_mode = match value.as_str() {
                                    "local" | "current" | "latest" => value,
                                    _ => "current".to_string(),
                                };
                            }
                            "payload_local_path" => {
                                config.payload_local_path = value;
                            }
                            "optimize_upload" => {
                                config.optimize_upload = matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                );
                            }
                            "chat_display_name" => {
                                config.chat_display_name = value;
                            }
                            "rar_extract_mode" => {
                                config.rar_extract_mode = match value.as_str() {
                                    "normal" | "safe" | "turbo" => value,
                                    _ => "turbo".to_string(),
                                };
                            }
                            "rar_safe_extract" => {
                                if matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                ) {
                                    config.rar_extract_mode = "safe".to_string();
                                }
                            }
                            "rar_turbo_extract" => {
                                if matches!(
                                    value.to_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                ) {
                                    config.rar_extract_mode = "turbo".to_string();
                                }
                            }
                            // Backwards compatibility: ignore removed fields
                            "extract_archives_fast" => {} // Removed fields are ignored
                            "rar_stream_on_the_fly" => {} // Removed fields are ignored
                            _ => {}                       // Removed fields are ignored
                        }
                    }
                }
                if config.auto_check_payload && !config.payload_auto_reload {
                    config.payload_auto_reload = true;
                }
                return config;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(Path::new("ps5upload.ini"))
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let content = format!(
            "address={}\nstorage={}\nconnections={}\nuse_temp={}\nauto_connect={}\ntheme={}\ncompression={}\nbandwidth_limit_mbps={}\nupdate_channel={}\ndownload_compression={}\nchmod_after_upload={}\noverride_on_conflict={}\nresume_mode={}\nlanguage={}\nauto_tune_connections={}\nauto_check_payload={}\npayload_auto_reload={}\npayload_reload_mode={}\npayload_local_path={}\noptimize_upload={}\nchat_display_name={}\n",
            self.address,
            self.storage,
            self.connections,
            self.use_temp,
            self.auto_connect,
            self.theme,
            self.compression,
            self.bandwidth_limit_mbps,
            self.update_channel,
            self.download_compression,
            self.chmod_after_upload,
            self.override_on_conflict,
            self.resume_mode,
            self.language,
            self.auto_tune_connections,
            self.auto_check_payload,
            self.payload_auto_reload,
            self.payload_reload_mode,
            self.payload_local_path,
            self.optimize_upload,
            self.chat_display_name,
        );
        let content = format!("{}rar_extract_mode={}\n", content, self.rar_extract_mode);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }
}
