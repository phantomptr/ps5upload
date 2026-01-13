use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub address: String,
    pub storage: String,
    pub connections: usize,
    pub use_temp: bool,
    pub auto_connect: bool,
    pub theme: String, // "dark" or "light"
    pub compression: String, // "none" or "lz4"
    pub bandwidth_limit_mbps: f32, // 0 = unlimited
    pub update_channel: String, // "stable" or "all"
    pub download_compression: String, // "none" or "lz4"
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            address: "192.168.0.100".to_string(),
            storage: "/data".to_string(),
            connections: 5, // Default to 5 connections for stability, max 10
            use_temp: false,
            auto_connect: false,
            theme: "dark".to_string(),
            compression: "none".to_string(),
            bandwidth_limit_mbps: 0.0,
            update_channel: "stable".to_string(),
            download_compression: "none".to_string(),
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        let path = Path::new("ps5upload.ini");
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
                                config.use_temp = matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
                            }
                            "auto_connect" => {
                                config.auto_connect = matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
                            }
                            "theme" => {
                                config.theme = if value == "light" { "light".to_string() } else { "dark".to_string() };
                            }
                            "compression" => {
                                config.compression = if value == "lz4" { "lz4".to_string() } else { "none".to_string() };
                            }
                            "bandwidth_limit_mbps" => {
                                if let Ok(parsed) = value.parse::<f32>() {
                                    config.bandwidth_limit_mbps = parsed.max(0.0);
                                }
                            }
                            "update_channel" => {
                                config.update_channel = if value == "all" { "all".to_string() } else { "stable".to_string() };
                            }
                            "download_compression" => {
                                config.download_compression = if value == "lz4" { "lz4".to_string() } else { "none".to_string() };
                            }
                            _ => {}
                        }
                    }
                }
                return config;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        let content = format!(
            "address={}\nstorage={}\nconnections={}\nuse_temp={}\nauto_connect={}\ntheme={}\ncompression={}\nbandwidth_limit_mbps={}\nupdate_channel={}\ndownload_compression={}\n",
            self.address,
            self.storage,
            self.connections,
            self.use_temp,
            self.auto_connect,
            self.theme,
            self.compression,
            self.bandwidth_limit_mbps,
            self.update_channel,
            self.download_compression
        );
        let _ = fs::write("ps5upload.ini", content);
    }
}
