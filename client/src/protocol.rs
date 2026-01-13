use anyhow::{Result, anyhow, Context};
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::{HashMap, VecDeque};
use lz4_flex::block::decompress_size_prepended;
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

pub async fn hash_file(ip: &str, port: u16, path: &str) -> Result<String> {
    let cmd = format!("HASH_FILE {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK ") {
        Ok(response.trim_start_matches("OK ").trim().to_string())
    } else {
        Err(anyhow!("Hash failed: {}", response))
    }
}

pub async fn list_dir_recursive(ip: &str, port: u16, base_path: &str) -> Result<HashMap<String, DirEntry>> {
    let base = base_path.trim_end_matches('/');
    let mut results: HashMap<String, DirEntry> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(base.to_string());

    while let Some(dir) = queue.pop_front() {
        let entries = list_dir(ip, port, &dir).await?;
        for entry in entries {
            let full_path = if dir == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            if entry.entry_type == "dir" {
                queue.push_back(full_path);
                continue;
            }
            let rel = full_path.strip_prefix(base).unwrap_or(&full_path);
            let rel = rel.trim_start_matches('/').to_string();
            results.insert(rel.clone(), DirEntry {
                name: rel,
                entry_type: entry.entry_type,
                size: entry.size,
                mtime: entry.mtime,
            });
        }
    }
    Ok(results)
}

pub async fn download_file_with_progress<F>(
    ip: &str,
    port: u16,
    path: &str,
    dest_path: &str,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<u64>
where
    F: FnMut(u64, u64, Option<String>),
{
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
    progress(0, total_size, Some(path.to_string()));

    let mut file = tokio::fs::File::create(dest_path).await?;
    let mut remaining = total_size;
    let mut buf = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    while remaining > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }
        let chunk = std::cmp::min(remaining, buf.len() as u64) as usize;
        stream.read_exact(&mut buf[..chunk]).await?;
        file.write_all(&buf[..chunk]).await?;
        remaining -= chunk as u64;
        received += chunk as u64;
        progress(received, total_size, None);
    }
    file.flush().await?;

    if remaining != 0 {
        return Err(anyhow!("Download incomplete"));
    }

    Ok(total_size)
}

pub async fn download_dir_with_progress<F>(
    ip: &str,
    port: u16,
    path: &str,
    dest_path: &str,
    cancel: Arc<AtomicBool>,
    use_lz4: bool,
    mut progress: F,
) -> Result<u64>
where
    F: FnMut(u64, u64, Option<String>),
{
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    let cmd = if use_lz4 {
        format!("DOWNLOAD_DIR {} LZ4\n", path)
    } else {
        format!("DOWNLOAD_DIR {}\n", path)
    };
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
    if !header_str.starts_with("READY") {
        return Err(anyhow!("Download failed: {}", header_str));
    }
    let total_size = header_str.split_whitespace().nth(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    progress(0, total_size, Some(path.to_string()));

    tokio::fs::create_dir_all(dest_path).await?;

    let mut current_path = String::new();
    let mut current_file: Option<tokio::fs::File> = None;
    let mut received = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }
        let mut header_buf = [0u8; 16];
        if let Err(err) = stream.read_exact(&mut header_buf).await {
            return Err(anyhow!("Download failed (header): {}", err));
        }
        let magic = u32::from_le_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]);
        if magic != 0x31585446 {
            return Err(anyhow!("Invalid frame magic"));
        }
        let frame_type = u32::from_le_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]);
        let body_len = u64::from_le_bytes([
            header_buf[8], header_buf[9], header_buf[10], header_buf[11],
            header_buf[12], header_buf[13], header_buf[14], header_buf[15],
        ]) as usize;

        if frame_type == 6 {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }
        if frame_type == 7 {
            let mut body = vec![0u8; body_len];
            stream.read_exact(&mut body).await?;
            let msg = String::from_utf8_lossy(&body).to_string();
            return Err(anyhow!("Download failed: {}", msg));
        }
        let raw_body = if frame_type == 8 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = stream.read_exact(&mut body).await {
                return Err(anyhow!("Download failed (body): {}", err));
            }
            decompress_size_prepended(&body).map_err(|e| anyhow!("Download failed (lz4): {}", e))?
        } else if frame_type == 4 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = stream.read_exact(&mut body).await {
                return Err(anyhow!("Download failed (body): {}", err));
            }
            body
        } else {
            return Err(anyhow!("Unexpected frame type: {}", frame_type));
        };

        if raw_body.len() < 4 {
            return Err(anyhow!("Invalid pack"));
        }
        let record_count = u32::from_le_bytes([raw_body[0], raw_body[1], raw_body[2], raw_body[3]]);
        let mut offset = 4usize;
        let body_len = raw_body.len();

        for _ in 0..record_count {
            if offset + 2 > body_len {
                return Err(anyhow!("Invalid record"));
            }
            let path_len = u16::from_le_bytes([raw_body[offset], raw_body[offset + 1]]) as usize;
            offset += 2;
            if offset + path_len + 8 > body_len {
                return Err(anyhow!("Invalid record"));
            }
            let rel_path = String::from_utf8_lossy(&raw_body[offset..offset + path_len]).to_string();
            if rel_path.contains('\0') {
                return Err(anyhow!("Download failed: corrupted stream (unexpected NUL in path). Disable compression and retry."));
            }
            offset += path_len;
            let data_len = u64::from_le_bytes([
                raw_body[offset], raw_body[offset + 1], raw_body[offset + 2], raw_body[offset + 3],
                raw_body[offset + 4], raw_body[offset + 5], raw_body[offset + 6], raw_body[offset + 7],
            ]) as usize;
            offset += 8;
            if offset + data_len > body_len {
                return Err(anyhow!("Invalid record data"));
            }

            let full_path = std::path::Path::new(dest_path).join(&rel_path);
            if rel_path != current_path {
                if let Some(mut file) = current_file.take() {
                    file.flush().await.ok();
                }
                if let Some(parent) = full_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                current_file = Some(tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&full_path)
                    .await?);
                current_path = rel_path.clone();
            } else if current_file.is_none() {
                if let Some(parent) = full_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                current_file = Some(tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full_path)
                    .await?);
            }

            if let Some(file) = current_file.as_mut() {
                file.write_all(&raw_body[offset..offset + data_len]).await?;
            } else if data_len == 0 {
                tokio::fs::File::create(&full_path).await?;
            }
            offset += data_len;
            received += data_len as u64;
            progress(received, total_size, Some(rel_path));
        }
    }

    if let Some(mut file) = current_file {
        file.flush().await.ok();
    }

    Ok(received)
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

pub async fn get_payload_version(ip: &str, port: u16) -> Result<String> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr)
    ).await.context("Connection timed out")??;

    stream.write_all(b"VERSION\n").await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 128];
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

    let response_str = String::from_utf8_lossy(&response).trim().to_string();
    if response_str.starts_with("VERSION ") {
        Ok(response_str.trim_start_matches("VERSION ").trim().to_string())
    } else {
        Err(anyhow!("Unexpected response: {}", response_str))
    }
}
