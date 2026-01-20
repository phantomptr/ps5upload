/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Application message types for async communication between threads and the main loop.

use crate::chat::ChatMessage;
use crate::protocol::{DirEntry, StorageLocation};

/// Side of the file manager panel
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManageSide {
    Left,
    Right,
}

/// Chat connection status events
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatStatusEvent {
    Connected,
    Disconnected,
}

/// A pending update ready to be applied
#[derive(Clone, Debug)]
pub struct PendingUpdate {
    pub target_path: std::path::PathBuf,
    pub replacement_path: std::path::PathBuf,
    pub restart_path: std::path::PathBuf,
    pub is_dir: bool,
}

/// Release information from GitHub
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
    pub prerelease: bool,
}

/// Asset information from a GitHub release
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// Request to move a file/folder
#[derive(Clone, Debug)]
pub struct MoveRequest {
    pub src: String,
    pub dst: String,
    pub op_name: String,
    pub dst_exists: bool,
}

/// Request to rename a file/folder
#[derive(Clone, Debug)]
pub struct RenameRequest {
    pub src: String,
    pub dst: String,
}

/// Upload optimization results
#[derive(Clone, Debug)]
pub struct UploadOptimization {
    pub compression: Option<crate::transfer::CompressionMode>,
    pub connections: Option<usize>,
    pub sample_files: Option<usize>,
    pub sample_bytes: Option<u64>,
}

/// Game metadata from param.json
#[derive(Clone, Debug)]
pub struct GameMeta {
    pub title: String,
    pub title_id: String,
    pub content_id: String,
    pub version: String,
}

/// Cover image data
#[derive(Clone, Debug)]
pub struct CoverImage {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// All async messages that can be sent to the main loop
#[derive(Debug)]
pub enum AppMessage {
    Log(String),
    PayloadLog(String),
    StatusPhase(String),
    ChatMessage(ChatMessage),
    ChatStatus(ChatStatusEvent),
    ChatAck {
        ok: bool,
        reason: Option<String>,
    },
    PayloadSendComplete(Result<u64, String>),
    PayloadVersion(Result<String, String>),
    StorageList(Result<Vec<StorageLocation>, String>),
    ManageList {
        side: ManageSide,
        result: Result<Vec<DirEntry>, String>,
    },
    ManageOpComplete {
        op: String,
        result: Result<(), String>,
    },
    ManageProgress {
        op: String,
        processed: u64,
        total: u64,
    },
    DownloadStart {
        total: u64,
        label: String,
    },
    DownloadProgress {
        received: u64,
        total: u64,
        current_file: Option<String>,
    },
    DownloadComplete(Result<u64, String>),
    UploadOptimizeComplete(UploadOptimization),
    MoveCheckResult {
        req: MoveRequest,
        exists: bool,
    },
    UpdateCheckComplete(Result<ReleaseInfo, String>),
    UpdateDownloadComplete {
        kind: String,
        result: Result<String, String>,
    },
    SelfUpdateReady(Result<PendingUpdate, String>),
    CheckExistsResult(bool),
    SizeCalculated(u64),
    Scanning {
        run_id: u64,
        files_found: usize,
        total_size: u64,
    },
    UploadStart {
        run_id: u64,
    },
    Progress {
        run_id: u64,
        sent: u64,
        total: u64,
        files_sent: i32,
        elapsed_secs: f64,
        current_file: Option<String>,
    },
    UploadComplete {
        run_id: u64,
        result: Result<(i32, u64), String>,
    },
    GameMetaLoaded {
        path: String,
        meta: Option<GameMeta>,
        cover: Option<CoverImage>,
    },
    ManageMetaLoaded {
        path: String,
        meta: Option<GameMeta>,
        cover: Option<CoverImage>,
    },
}
