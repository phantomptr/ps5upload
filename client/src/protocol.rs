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

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub size: u64,
    pub mtime: Option<i64>,
}

async fn send_simple_command(ip: &str, port: u16, cmd: &str) -> Result<String> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    stream.write_all(cmd.as_bytes()).await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..n]);
        if response.contains(&b'\n') {
            break;
        }
    }

    Ok(String::from_utf8_lossy(&response).trim().to_string())
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

pub async fn list_dir(ip: &str, port: u16, path: &str) -> Result<Vec<DirEntry>> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let cmd = format!("LIST_DIR {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..n]);
        if response.windows(3).any(|w| w == b"\n]\n") {
             break;
        }
    }

    let response_str = String::from_utf8(response)?;
    let json_end = response_str.rfind(']').ok_or_else(|| anyhow!("Invalid JSON response"))?;
    let json_str = &response_str[..=json_end];

    let entries: Vec<DirEntry> = serde_json::from_str(json_str)?;
    Ok(entries)
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

pub async fn delete_path(ip: &str, port: u16, path: &str) -> Result<()> {
    let cmd = format!("DELETE {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!("Delete failed: {}", response))
    }
}

pub async fn move_path(ip: &str, port: u16, src: &str, dst: &str) -> Result<()> {
    let cmd = format!("MOVE {}\t{}\n", src, dst);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!("Move failed: {}", response))
    }
}

pub async fn copy_path(ip: &str, port: u16, src: &str, dst: &str) -> Result<()> {
    let cmd = format!("COPY {}\t{}\n", src, dst);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!("Copy failed: {}", response))
    }
}

pub async fn chmod_777(ip: &str, port: u16, path: &str) -> Result<()> {
    let cmd = format!("CHMOD777 {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!("Chmod failed: {}", response))
    }
}

pub async fn download_file(ip: &str, port: u16, path: &str, dest_path: &str) -> Result<()> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let cmd = format!("DOWNLOAD {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut header = Vec::new();
    let mut buffer = [0u8; 1];
    while header.len() < 128 {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        header.push(buffer[0]);
        if buffer[0] == b'\n' {
            break;
        }
    }

    let header_str = String::from_utf8_lossy(&header).trim().to_string();
    if !header_str.starts_with("OK ") {
        return Err(anyhow!("Download failed: {}", header_str));
    }
    let size_str = header_str.trim_start_matches("OK ").trim();
    let total_size: u64 = size_str.parse().map_err(|_| anyhow!("Invalid size header"))?;

    let mut file = tokio::fs::File::create(dest_path).await?;
    let mut remaining = total_size;
    let mut buf = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let chunk = std::cmp::min(remaining, buf.len() as u64) as usize;
        stream.read_exact(&mut buf[..chunk]).await?;
        file.write_all(&buf[..chunk]).await?;
        remaining -= chunk as u64;
    }
    file.flush().await?;

    if remaining != 0 {
        return Err(anyhow!("Download incomplete"));
    }

    Ok(())
}

pub async fn upload_v2_init(ip: &str, port: u16, dest_path: &str, use_temp: bool) -> Result<TcpStream> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let mode = if use_temp { "TEMP" } else { "DIRECT" };
    let cmd = format!("UPLOAD_V2 {} {}\n", dest_path, mode);
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
