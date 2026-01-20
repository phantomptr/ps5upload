use anyhow::{anyhow, Context, Result};
use lz4_flex::block::decompress_size_prepended;
use lzma_rs::lzma_decompress;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zstd::bulk::decompress as zstd_decompress;

pub const CONNECTION_TIMEOUT_SECS: u64 = 30;
const READ_TIMEOUT_SECS: u64 = 120;

async fn read_timeout(stream: &mut TcpStream, buf: &mut [u8]) -> Result<usize> {
    tokio::time::timeout(
        std::time::Duration::from_secs(READ_TIMEOUT_SECS),
        stream.read(buf),
    )
    .await
    .context("Read timed out")?
    .map_err(Into::into)
}

async fn read_exact_timeout(stream: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(READ_TIMEOUT_SECS),
        stream.read_exact(buf),
    )
    .await
    .context("Read timed out")??;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StorageLocation {
    pub path: String,
    #[serde(rename = "type")]
    pub storage_type: String,
    pub free_gb: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub size: u64,
    pub mtime: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DownloadCompression {
    None,
    Lz4,
    Zstd,
    Lzma,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RarExtractMode {
    Normal,
    Safe,
    Turbo,
}

async fn send_simple_command(ip: &str, port: u16, cmd: &str) -> Result<String> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    stream.write_all(cmd.as_bytes()).await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let n = read_timeout(&mut stream, &mut buffer).await?;
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
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    stream.write_all(b"LIST_STORAGE\n").await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        let n = read_timeout(&mut stream, &mut buffer).await?;
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
    let json_end = response_str
        .rfind(']')
        .ok_or_else(|| anyhow!("Invalid JSON response"))?;
    let json_str = &response_str[..=json_end];

    let locations: Vec<StorageLocation> = serde_json::from_str(json_str)?;
    Ok(locations)
}

pub async fn list_dir(ip: &str, port: u16, path: &str) -> Result<Vec<DirEntry>> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let cmd = format!("LIST_DIR {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        let n = read_timeout(&mut stream, &mut buffer).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..n]);
        if response.windows(3).any(|w| w == b"\n]\n") {
            break;
        }
    }

    let response_str = String::from_utf8(response)?;
    let json_end = response_str
        .rfind(']')
        .ok_or_else(|| anyhow!("Invalid JSON response"))?;
    let json_str = &response_str[..=json_end];

    let entries: Vec<DirEntry> = serde_json::from_str(json_str)?;
    Ok(entries)
}

pub async fn check_dir(ip: &str, port: u16, path: &str) -> Result<bool> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let cmd = format!("CHECK_DIR {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut buffer = [0u8; 1024];
    let n = read_timeout(&mut stream, &mut buffer).await?;
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

pub async fn probe_rar_metadata(
    ip: &str,
    port: u16,
    path: &str,
) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let cmd = format!("PROBE_RAR {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut reader = tokio::io::BufReader::new(stream);
    let timeout = std::time::Duration::from_secs(READ_TIMEOUT_SECS);
    let mut line = String::new();
    let mut param: Option<Vec<u8>> = None;
    let mut cover: Option<Vec<u8>> = None;

    loop {
        line.clear();
        let n = tokio::time::timeout(timeout, reader.read_line(&mut line))
            .await
            .context("Read timed out")??;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.starts_with("ERROR") {
            return Err(anyhow!("Probe failed: {}", trimmed));
        }
        if let Some(size_str) = trimmed.strip_prefix("META ") {
            let size: usize = size_str.trim().parse().unwrap_or(0);
            if size > 0 {
                let mut buf = vec![0u8; size];
                tokio::time::timeout(timeout, reader.read_exact(&mut buf))
                    .await
                    .context("Read timed out")??;
                let mut newline = [0u8; 1];
                let _ = tokio::time::timeout(timeout, reader.read_exact(&mut newline)).await;
                param = Some(buf);
            }
            continue;
        }
        if let Some(size_str) = trimmed.strip_prefix("COVER ") {
            let size: usize = size_str.trim().parse().unwrap_or(0);
            if size > 0 {
                let mut buf = vec![0u8; size];
                tokio::time::timeout(timeout, reader.read_exact(&mut buf))
                    .await
                    .context("Read timed out")??;
                let mut newline = [0u8; 1];
                let _ = tokio::time::timeout(timeout, reader.read_exact(&mut newline)).await;
                cover = Some(buf);
            }
            continue;
        }
        if trimmed == "DONE" {
            break;
        }
    }

    Ok((param, cover))
}

async fn send_manage_command_with_progress<F>(
    ip: &str,
    port: u16,
    cmd: &str,
    prefix: &str,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    stream.write_all(cmd.as_bytes()).await?;

    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    let timeout = std::time::Duration::from_secs(600);
    let mut last_recv = std::time::Instant::now();

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = reader.get_mut().write_all(b"CANCEL\n").await;
            return Err(anyhow!("Cancelled"));
        }
        line.clear();
        let n = tokio::select! {
            res = reader.read_line(&mut line) => {
                res.map_err(|e| anyhow!("Failed to read from server: {}", e))?
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = reader.get_mut().write_all(b"CANCEL\n").await;
                    return Err(anyhow!("Cancelled"));
                }
                if last_recv.elapsed() > timeout {
                    return Err(anyhow!("Operation timed out (no progress for 10m)"));
                }
                continue;
            }
        };
        if n == 0 {
            return Err(anyhow!("Connection closed by server"));
        }
        last_recv = std::time::Instant::now();
        let trimmed = line.trim();
        if trimmed == "OK" || trimmed.starts_with("OK ") {
            return Ok(());
        }
        if trimmed.starts_with("ERROR: ") {
            return Err(anyhow!("{}", trimmed));
        }
        if trimmed.starts_with(prefix) {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 4 {
                let processed: u64 = parts[2].parse().unwrap_or(0);
                let total: u64 = parts[3].parse().unwrap_or(0);
                progress(processed, total);
            } else if parts.len() >= 3 {
                let processed: u64 = parts[1].parse().unwrap_or(0);
                let total: u64 = parts[2].parse().unwrap_or(0);
                progress(processed, total);
            }
        }
    }
}

pub async fn move_path_with_progress<F>(
    ip: &str,
    port: u16,
    src: &str,
    dst: &str,
    cancel: Arc<AtomicBool>,
    progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let cmd = format!("MOVE {}\t{}\n", src, dst);
    send_manage_command_with_progress(ip, port, &cmd, "MOVE_PROGRESS", cancel, progress).await
}

pub async fn copy_path_with_progress<F>(
    ip: &str,
    port: u16,
    src: &str,
    dst: &str,
    cancel: Arc<AtomicBool>,
    progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let cmd = format!("COPY {}\t{}\n", src, dst);
    send_manage_command_with_progress(ip, port, &cmd, "COPY_PROGRESS", cancel, progress).await
}

pub async fn extract_archive_with_progress<F>(
    ip: &str,
    port: u16,
    src: &str,
    dst: &str,
    cancel: Arc<AtomicBool>,
    progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let cmd = format!("EXTRACT_ARCHIVE {}\t{}\n", src, dst);
    send_manage_command_with_progress(ip, port, &cmd, "EXTRACT_PROGRESS", cancel, progress).await
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

pub async fn chmod_777(ip: &str, port: u16, path: &str) -> Result<()> {
    let cmd = format!("CHMOD777 {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK") {
        Ok(())
    } else {
        Err(anyhow!("Chmod failed: {}", response))
    }
}

pub async fn create_path(ip: &str, port: u16, path: &str) -> Result<()> {
    let cmd = format!("CREATE_PATH {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("SUCCESS") {
        Ok(())
    } else {
        Err(anyhow!("Create folder failed: {}", response))
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

pub async fn get_space(ip: &str, port: u16, path: &str) -> Result<(u64, u64)> {
    let cmd = format!("GET_SPACE {}\n", path);
    let response = send_simple_command(ip, port, &cmd).await?;
    if response.starts_with("OK ") {
        let parts: Vec<&str> = response.split_whitespace().collect();
        if parts.len() >= 3 {
            let free: u64 = parts[1].parse().unwrap_or(0);
            let total: u64 = parts[2].parse().unwrap_or(0);
            return Ok((free, total));
        }
    }
    Err(anyhow!("Failed to get space: {}", response))
}

pub async fn list_dir_recursive(
    ip: &str,
    port: u16,
    base_path: &str,
) -> Result<HashMap<String, DirEntry>> {
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
            results.insert(
                rel.clone(),
                DirEntry {
                    name: rel,
                    entry_type: entry.entry_type,
                    size: entry.size,
                    mtime: entry.mtime,
                },
            );
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
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let cmd = format!("DOWNLOAD {}\n", path);
    stream.write_all(cmd.as_bytes()).await?;

    let mut header = Vec::new();
    let mut buffer = [0u8; 1];
    while header.len() < 128 {
        let n = read_timeout(&mut stream, &mut buffer).await?;
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
    let total_size: u64 = size_str
        .parse()
        .map_err(|_| anyhow!("Invalid size header"))?;
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
        read_exact_timeout(&mut stream, &mut buf[..chunk]).await?;
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

pub async fn download_dir_with_progress<F, G>(
    ip: &str,
    port: u16,
    path: &str,
    dest_path: &str,
    cancel: Arc<AtomicBool>,
    compression: DownloadCompression,
    mut progress: F,
    mut info: G,
) -> Result<u64>
where
    F: FnMut(u64, u64, Option<String>),
    G: FnMut(Option<String>),
{
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let cmd = match compression {
        DownloadCompression::Lz4 => format!("DOWNLOAD_DIR {} LZ4\n", path),
        DownloadCompression::Zstd => format!("DOWNLOAD_DIR {} ZSTD\n", path),
        DownloadCompression::Lzma => format!("DOWNLOAD_DIR {} LZMA\n", path),
        DownloadCompression::Auto => format!("DOWNLOAD_DIR {} AUTO\n", path),
        DownloadCompression::None => format!("DOWNLOAD_DIR {}\n", path),
    };
    stream.write_all(cmd.as_bytes()).await?;

    let mut header = Vec::new();
    let mut buffer = [0u8; 1];
    while header.len() < 128 {
        let n = read_timeout(&mut stream, &mut buffer).await?;
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
    let mut parts = header_str.split_whitespace();
    let _ = parts.next(); // READY
    let total_size = parts
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut comp: Option<String> = None;
    while let Some(part) = parts.next() {
        if part == "COMP" {
            comp = parts.next().map(|s| s.to_string());
            break;
        }
    }
    info(comp);
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
        if let Err(err) = read_exact_timeout(&mut stream, &mut header_buf).await {
            return Err(anyhow!("Download failed (header): {}", err));
        }
        let magic =
            u32::from_le_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]);
        if magic != 0x31585446 {
            return Err(anyhow!("Invalid frame magic"));
        }
        let frame_type =
            u32::from_le_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]);
        let body_len = u64::from_le_bytes([
            header_buf[8],
            header_buf[9],
            header_buf[10],
            header_buf[11],
            header_buf[12],
            header_buf[13],
            header_buf[14],
            header_buf[15],
        ]) as usize;

        if frame_type == 6 {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }
        if frame_type == 7 {
            let mut body = vec![0u8; body_len];
            read_exact_timeout(&mut stream, &mut body).await?;
            let msg = String::from_utf8_lossy(&body).to_string();
            return Err(anyhow!("Download failed: {}", msg));
        }
        let raw_body = if frame_type == 8 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = read_exact_timeout(&mut stream, &mut body).await {
                return Err(anyhow!("Download failed (body): {}", err));
            }
            decompress_size_prepended(&body).map_err(|e| anyhow!("Download failed (lz4): {}", e))?
        } else if frame_type == 9 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = read_exact_timeout(&mut stream, &mut body).await {
                return Err(anyhow!("Download failed (body): {}", err));
            }
            if body.len() < 4 {
                return Err(anyhow!("Download failed (zstd): invalid header"));
            }
            let raw_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            let decompressed = zstd_decompress(&body[4..], raw_len)
                .map_err(|e| anyhow!("Download failed (zstd): {}", e))?;
            decompressed
        } else if frame_type == 10 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = read_exact_timeout(&mut stream, &mut body).await {
                return Err(anyhow!("Download failed (body): {}", err));
            }
            if body.len() < 4 {
                return Err(anyhow!("Download failed (lzma): invalid header"));
            }
            let raw_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            let cursor = std::io::Cursor::new(&body[4..]);
            let mut input = std::io::BufReader::new(cursor);
            let mut out = Vec::with_capacity(raw_len);
            lzma_decompress(&mut input, &mut out)
                .map_err(|e| anyhow!("Download failed (lzma): {}", e))?;
            if out.len() != raw_len {
                return Err(anyhow!("Download failed (lzma): size mismatch"));
            }
            out
        } else if frame_type == 4 {
            let mut body = vec![0u8; body_len];
            if let Err(err) = read_exact_timeout(&mut stream, &mut body).await {
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
            let rel_path =
                String::from_utf8_lossy(&raw_body[offset..offset + path_len]).to_string();
            if rel_path.contains('\0') {
                return Err(anyhow!("Download failed: corrupted stream (unexpected NUL in path). Disable compression and retry."));
            }
            offset += path_len;
            let data_len = u64::from_le_bytes([
                raw_body[offset],
                raw_body[offset + 1],
                raw_body[offset + 2],
                raw_body[offset + 3],
                raw_body[offset + 4],
                raw_body[offset + 5],
                raw_body[offset + 6],
                raw_body[offset + 7],
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
                current_file = Some(
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&full_path)
                        .await?,
                );
                current_path = rel_path.clone();
            } else if current_file.is_none() {
                if let Some(parent) = full_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                current_file = Some(
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&full_path)
                        .await?,
                );
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

pub async fn upload_v2_init(
    ip: &str,
    port: u16,
    dest_path: &str,
    use_temp: bool,
) -> Result<TcpStream> {
    let addr = format!("{}:{}", ip, port);
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    let mode = if use_temp { "TEMP" } else { "DIRECT" };
    let cmd = format!("UPLOAD_V2 {} {}\n", dest_path, mode);
    stream.write_all(cmd.as_bytes()).await?;

    // Wait for READY
    let mut buffer = [0u8; 1024];
    let n = read_timeout(&mut stream, &mut buffer).await?;
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
        TcpStream::connect(&addr),
    )
    .await
    .context("Connection timed out")??;

    stream.write_all(b"VERSION\n").await?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 128];
    loop {
        let n = read_timeout(&mut stream, &mut buffer).await?;
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
        Ok(response_str
            .trim_start_matches("VERSION ")
            .trim()
            .to_string())
    } else {
        Err(anyhow!("Unexpected response: {}", response_str))
    }
}

/// Upload a RAR file to PS5 for server-side extraction
/// This sends the RAR file directly and the PS5 payload extracts it
pub async fn upload_rar_for_extraction<F, P>(
    ip: &str,
    port: u16,
    rar_path: &str,
    dest_path: &str,
    mode: RarExtractMode,
    cancel: Arc<AtomicBool>,
    mut progress: F,
    mut extract_progress: P,
) -> Result<(u32, u64)>
where
    F: FnMut(u64, u64),
    P: FnMut(String),
{
    // Get file size
    let metadata = tokio::fs::metadata(rar_path)
        .await
        .context("Failed to read RAR file metadata")?;
    let file_size = metadata.len();

    let addr = format!("{}:{}", ip, port);
    let mut tried_fallback = false;
    let mut mode_to_try = mode;
    let stream = loop {
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(CONNECTION_TIMEOUT_SECS),
            TcpStream::connect(&addr),
        )
        .await
        .context("Connection timed out")??;

        // Send command
        let cmd = match mode_to_try {
            RarExtractMode::Safe => format!("UPLOAD_RAR_SAFE {} {}\n", dest_path, file_size),
            RarExtractMode::Turbo => format!("UPLOAD_RAR_TURBO {} {}\n", dest_path, file_size),
            RarExtractMode::Normal => format!("UPLOAD_RAR {} {}\n", dest_path, file_size),
        };
        stream.write_all(cmd.as_bytes()).await?;

        // Wait for READY
        let mut buffer = [0u8; 1024];
        let n = read_timeout(&mut stream, &mut buffer).await?;
        let response = String::from_utf8_lossy(&buffer[..n]).trim().to_string();

        if response == "READY" {
            break stream;
        }

        if !tried_fallback
            && mode_to_try != RarExtractMode::Normal
            && response.contains("Unknown command")
        {
            tried_fallback = true;
            mode_to_try = RarExtractMode::Normal;
            extract_progress("RAR mode unsupported by payload. Retrying with Normal.".to_string());
            continue;
        }

        return Err(anyhow!("Server rejected RAR upload: {}", response));
    };

    // Send RAR file data
    let mut file = tokio::fs::File::open(rar_path).await?;
    let mut sent = 0u64;
    let mut read_buf = vec![0u8; 256 * 1024]; // 256KB chunks

    // Buffer for reading potential error responses during upload
    let mut response_buf = [0u8; 1024];

    // Split stream to allow concurrent read/write monitoring
    let (mut rd, mut wr) = stream.into_split();

    while sent < file_size {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }

        let n = file.read(&mut read_buf).await?;
        if n == 0 {
            break;
        }

        tokio::select! {
            write_res = wr.write_all(&read_buf[..n]) => {
                write_res?;
                sent += n as u64;
                progress(sent, file_size);
            }
            read_res = rd.read(&mut response_buf) => {
                match read_res {
                    Ok(0) => return Err(anyhow!("Connection closed by server during upload")),
                    Ok(len) => {
                        let msg = String::from_utf8_lossy(&response_buf[..len]).to_string();
                        return Err(anyhow!("Server error: {}", msg.trim()));
                    }
                    Err(e) => return Err(anyhow!("Failed to read from server: {}", e)),
                }
            }
        }
    }

    // Wait for extraction result (may take a while for large archives)
    // Use BufReader for line-based reading to handle EXTRACTING messages
    let mut reader = tokio::io::BufReader::new(rd);
    let mut line = String::new();
    let timeout = std::time::Duration::from_secs(600); // 10 minute timeout between messages

    loop {
        line.clear();
        let read_future = reader.read_line(&mut line);

        let n = match tokio::time::timeout(timeout, read_future).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(anyhow!("Failed to read from server: {}", e)),
            Err(_) => return Err(anyhow!("Extraction timed out (no progress for 10m)")),
        };

        if n == 0 {
            return Err(anyhow!("Connection closed during extraction"));
        }

        let trimmed = line.trim();
        if trimmed.starts_with("SUCCESS ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let files: u32 = parts[1].parse().unwrap_or(0);
                let bytes: u64 = parts[2].parse().unwrap_or(0);
                return Ok((files, bytes));
            } else {
                return Ok((0, 0));
            }
        } else if trimmed.starts_with("ERROR: ") {
            return Err(anyhow!("RAR extraction failed: {}", trimmed));
        } else if trimmed.starts_with("EXTRACT_PROGRESS ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 4 {
                let processed: u64 = parts[2].parse().unwrap_or(0);
                let total: u64 = parts[3].parse().unwrap_or(0);
                progress(processed, total);

                if parts.len() >= 5 {
                    // Extract filename from the rest of the string
                    // Find the position after the 4th token (total)
                    // This is robust against spaces in filenames
                    let mut current_pos = 0;
                    for _ in 0..4 {
                        if let Some(pos) = trimmed[current_pos..].find(char::is_whitespace) {
                            current_pos += pos;
                            if let Some(next_char) =
                                trimmed[current_pos..].find(|c: char| !c.is_whitespace())
                            {
                                current_pos += next_char;
                            }
                        }
                    }
                    // Actually simpler: just find the index of parts[3] and add its len
                    // But parts[3] might appear earlier (unlikely for numbers but still)
                    // Let's use the parts slice
                    // Reconstruct filename? No.
                    // Let's iterate tokens.
                    // Or simpler: use splitn(5)
                    let parts_n: Vec<&str> = trimmed.splitn(5, char::is_whitespace).collect();
                    if parts_n.len() >= 5 {
                        let filename = parts_n[4].trim();
                        if !filename.is_empty() {
                            extract_progress(format!("Extracting: {}", filename));
                        }
                    }
                }
            }
        } else if trimmed.starts_with("EXTRACTING ") {
            if let Some(rest) = trimmed.strip_prefix("EXTRACTING ") {
                if let Some((count, filename)) = rest.split_once(' ') {
                    extract_progress(format!("Extracting ({}): {}", count, filename));
                }
            }
        }
    }
}
