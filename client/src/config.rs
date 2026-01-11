use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub address: String,
    pub storage: String,
    pub connections: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            address: "192.168.0.100".to_string(),
            storage: "/data".to_string(),
            connections: 1,
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
            "address={}\nstorage={}\nconnections={}\n",
            self.address, self.storage, self.connections
        );
        let _ = fs::write("ps5upload.ini", content);
    }
}
