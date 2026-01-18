/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Shared transfer helpers (sampling, compression choice, response parsing).

use crate::message::UploadOptimization;
use crate::transfer::{CompressionMode, FileEntry};
use anyhow::Result;
use lz4_flex::block::compress_prepend_size;
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use walkdir::WalkDir;

pub fn sample_workload(path: &str, cancel: &Arc<AtomicBool>) -> Option<(usize, u64)> {
    let path = Path::new(path);
    if path.is_file() {
        let size = path.metadata().ok().map(|m| m.len()).unwrap_or(0);
        return Some((1, size));
    }
    let mut count = 0usize;
    let mut total = 0u64;
    let start = std::time::Instant::now();
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if start.elapsed() > std::time::Duration::from_millis(300) {
            break;
        }
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
        count += 1;
        if count >= 2000 {
            break;
        }
    }
    if count == 0 {
        None
    } else {
        Some((count, total))
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}

pub fn sample_bytes_from_path(path: &str, cancel: &Arc<AtomicBool>) -> Option<Vec<u8>> {
    let path = Path::new(path);
    let max_total = 512 * 1024;
    let per_file = 64 * 1024;
    let mut out = Vec::with_capacity(max_total.min(64 * 1024));
    let mut total = 0usize;

    if path.is_file() {
        if !cancel.load(Ordering::Relaxed) && total < max_total {
            if let Ok(mut file) = std::fs::File::open(path) {
                let remaining = max_total - total;
                let to_read = per_file.min(remaining);
                let mut buf = vec![0u8; to_read];
                if let Ok(n) = file.read(&mut buf) {
                    if n > 0 {
                        out.extend_from_slice(&buf[..n]);
                    }
                }
            }
        }
        return if out.is_empty() { None } else { Some(out) };
    }

    let start = std::time::Instant::now();
    let mut files_seen = 0usize;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if start.elapsed() > std::time::Duration::from_millis(400) {
            break;
        }
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        if total < max_total {
            if let Ok(mut file) = std::fs::File::open(entry_path) {
                let remaining = max_total - total;
                let to_read = per_file.min(remaining);
                let mut buf = vec![0u8; to_read];
                if let Ok(n) = file.read(&mut buf) {
                    if n > 0 {
                        out.extend_from_slice(&buf[..n]);
                        total += n;
                    }
                }
            }
        }
        files_seen += 1;
        if total >= max_total || files_seen >= 200 {
            break;
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn sample_bytes_from_files(files: &[FileEntry], cancel: &Arc<AtomicBool>) -> Option<Vec<u8>> {
    let max_total = 512 * 1024;
    let per_file = 64 * 1024;
    let mut out = Vec::with_capacity(max_total.min(64 * 1024));
    let mut total = 0usize;

    for entry in files.iter() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if total >= max_total {
            break;
        }
        if let Ok(mut file) = std::fs::File::open(&entry.abs_path) {
            let remaining = max_total - total;
            let to_read = per_file.min(remaining);
            let mut buf = vec![0u8; to_read];
            if let Ok(n) = file.read(&mut buf) {
                if n > 0 {
                    out.extend_from_slice(&buf[..n]);
                    total += n;
                }
            }
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn optimize_upload_settings(
    path: &str,
    cancel: &Arc<AtomicBool>,
    current_connections: usize,
) -> UploadOptimization {
    let mut opt = UploadOptimization {
        compression: None,
        connections: None,
        sample_files: None,
        sample_bytes: None,
    };

    if let Some((count, bytes)) = sample_workload(path, cancel) {
        opt.sample_files = Some(count);
        opt.sample_bytes = Some(bytes);
        opt.connections = Some(recommend_connections(current_connections, count, bytes));
    }

    if let Some(sample) = sample_bytes_from_path(path, cancel) {
        opt.compression = Some(choose_best_compression(&sample));
    }

    opt
}

pub fn choose_best_compression(sample: &[u8]) -> CompressionMode {
    let raw_size = sample.len();
    if raw_size == 0 {
        return CompressionMode::None;
    }

    let lz4 = compress_prepend_size(sample);
    let lz4_size = lz4.len();
    if lz4_size < raw_size {
        CompressionMode::Lz4
    } else {
        CompressionMode::None
    }
}

pub fn compression_label(mode: CompressionMode) -> &'static str {
    match mode {
        CompressionMode::None => "None",
        CompressionMode::Lz4 => "LZ4",
        CompressionMode::Zstd => "Zstd",
        CompressionMode::Lzma => "LZMA",
    }
}

pub fn compression_mode_to_setting(mode: CompressionMode) -> &'static str {
    match mode {
        CompressionMode::None => "none",
        CompressionMode::Lz4 => "lz4",
        CompressionMode::Zstd => "zstd",
        CompressionMode::Lzma => "lzma",
    }
}

pub fn recommend_connections(current: usize, sample_count: usize, sample_bytes: u64) -> usize {
    if sample_count < 200 || current <= 1 {
        return current;
    }
    let avg = sample_bytes / sample_count as u64;
    if avg < 8 * 1024 {
        1
    } else if avg < 64 * 1024 {
        current.min(2)
    } else if avg < 512 * 1024 {
        current.min(4)
    } else {
        current
    }
}

pub fn parse_upload_response(response: &str) -> Result<(i32, u64)> {
    if response.starts_with("SUCCESS") {
        let parts: Vec<&str> = response.split_whitespace().collect();
        let files = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let bytes = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok((files, bytes))
    } else {
        Err(anyhow::anyhow!("Upload failed: {}", response))
    }
}

pub fn read_upload_response(stream: &mut TcpStream, cancel: &Arc<AtomicBool>) -> Result<String> {
    let start = std::time::Instant::now();
    let mut buffer = [0u8; 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Upload cancelled"));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Err(anyhow::anyhow!("Socket closed during response read")),
            Ok(n) => return Ok(String::from_utf8_lossy(&buffer[..n]).trim().to_string()),
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
                    if start.elapsed() >= std::time::Duration::from_secs(10) {
                        return Err(anyhow::anyhow!("Timed out waiting for server response"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                _ => return Err(err.into()),
            },
        }
    }
}

pub fn partition_files_by_size(mut files: Vec<FileEntry>, connections: usize) -> Vec<Vec<FileEntry>> {
    let connections = connections.max(1);
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    let mut buckets: Vec<Vec<FileEntry>> = vec![Vec::new(); connections];
    let mut bucket_sizes = vec![0u64; connections];

    for file in files {
        let (idx, _) = bucket_sizes
            .iter()
            .enumerate()
            .min_by_key(|(_, size)| *size)
            .unwrap();
        bucket_sizes[idx] += file.size;
        buckets[idx].push(file);
    }

    buckets
}

pub fn payload_supports_modern_compression(version: Option<&str>) -> bool {
    let Some(version) = version else {
        return false;
    };
    let current_norm = normalize_version("1.1.6");
    let running_norm = normalize_version(version);
    match (
        semver::Version::parse(&running_norm),
        semver::Version::parse(&current_norm),
    ) {
        (Ok(running), Ok(required)) => running >= required,
        _ => false,
    }
}

fn normalize_version(version: &str) -> String {
    version.trim_start_matches('v').trim().to_string()
}
