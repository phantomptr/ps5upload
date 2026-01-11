use anyhow::{Result, anyhow, Context};
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize};

pub const CONNECTION_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct StorageLocation {
    pub path: String,
    #[serde(rename = "type")]
    pub storage_type: String,
    pub free_gb: f64,
}

pub async fn list_storage(ip: &str, port: u16) -> Result<Vec<StorageLocation>> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    stream.write_all(b"LIST_STORAGE\n").await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..n]);
        // Check for end marker "\n]\n"
        if response.windows(3).any(|w| w == b"\n]\n") {
             break;
        }
    }

    let response_str = String::from_utf8(response)?;
    // Ideally we trim any trailing garbage if any
    let json_end = response_str.rfind(']').ok_or_else(|| anyhow!("Invalid JSON response"))?;
    let json_str = &response_str[..=json_end];

    let locations: Vec<StorageLocation> = serde_json::from_str(json_str)?;
    Ok(locations)
}

pub async fn check_dir(ip: &str, port: u16, path: &str) -> Result<bool> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let cmd = format!("CHECK_DIR {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    let response = String::from_utf8_lossy(&buffer[..n]).trim().to_string();

    Ok(response == "EXISTS")
}

pub async fn upload_v2_init(ip: &str, port: u16, dest_path: &str) -> Result<TcpStream> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let cmd = format!("UPLOAD_V2 {}\n", dest_path);
    stream.write_all(cmd.as_bytes()).await?;
    
    // Wait for READY
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    let response = String::from_utf8_lossy(&buffer[..n]).trim().to_string();
    
    if response == "READY" {
        Ok(stream)
    } else {
        Err(anyhow!("Server rejected V2 upload: {}", response))
    }
}
