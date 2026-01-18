/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Dialog state and rendering for modal dialogs.

pub mod archive_confirm;
pub mod delete_confirm;
pub mod destination_picker;
pub mod download_confirm;
pub mod history_resume;
pub mod move_confirm;
pub mod override_dialog;
pub mod profile_manager;
pub mod rename_confirm;
pub mod resume_dialog;
pub mod update_prompt;

use crate::history::TransferRecord;
use crate::message::{MoveRequest, RenameRequest};
use crate::protocol::DownloadCompression;

/// Request for downloading a file or directory
#[derive(Clone)]
pub enum DownloadRequest {
    File {
        name: String,
        target: String,
        save_path: String,
    },
    Dir {
        name: String,
        target: String,
        dest_root: String,
        compression: DownloadCompression,
    },
}

/// State for all modal dialogs
#[derive(Default)]
pub struct DialogState {
    // Override/Resume dialogs
    pub show_override: bool,
    pub show_resume: bool,
    pub force_full_upload_once: bool,

    // Download confirmation
    pub show_download_overwrite: bool,
    pub pending_download_request: Option<DownloadRequest>,

    // Move confirmation
    pub show_move_overwrite: bool,
    pub pending_move_request: Option<MoveRequest>,

    // Delete confirmation
    pub show_delete_confirm: bool,
    pub pending_delete_target: Option<String>,

    // Rename confirmation
    pub show_rename_confirm: bool,
    pub pending_rename_request: Option<RenameRequest>,

    // Archive confirmation
    pub show_archive_confirm: bool,
    pub pending_archive_path: Option<String>,
    pub pending_archive_kind: Option<String>,
    pub pending_archive_trim: bool,
    pub show_archive_overwrite: bool,
    pub archive_overwrite_confirmed: bool,

    // Update dialogs
    pub show_update_restart: bool,
    pub show_update_prompt: bool,
    pub update_prompt_snooze_until: Option<std::time::Instant>,

    // History resume dialog
    pub show_history_resume: bool,
    pub pending_history_record: Option<TransferRecord>,
    pub history_resume_mode: String,

    // Profile management dialog
    pub show_profile: bool,
    pub profile_name_input: String,

    // Destination picker (for manage operations)
    pub show_dest_picker: bool,
    pub dest_action: Option<ManageDestAction>,
    pub dest_source_path: Option<String>,
    pub dest_source_name: Option<String>,
}

/// Action for destination picker in manage tab
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManageDestAction {
    Move,
    Copy,
    Extract,
}

impl DialogState {
    pub fn new() -> Self {
        Self {
            pending_archive_trim: true,
            archive_overwrite_confirmed: true,
            history_resume_mode: "size".to_string(),
            ..Default::default()
        }
    }

    /// Check if any modal dialog is open
    pub fn any_open(&self) -> bool {
        self.show_override
            || self.show_resume
            || self.show_download_overwrite
            || self.show_move_overwrite
            || self.show_delete_confirm
            || self.show_rename_confirm
            || self.show_archive_confirm
            || self.show_archive_overwrite
            || self.show_update_restart
            || self.show_update_prompt
            || self.show_history_resume
            || self.show_profile
            || self.show_dest_picker
    }

    /// Close all dialogs
    pub fn close_all(&mut self) {
        self.show_override = false;
        self.show_resume = false;
        self.show_download_overwrite = false;
        self.show_move_overwrite = false;
        self.show_delete_confirm = false;
        self.show_rename_confirm = false;
        self.show_archive_confirm = false;
        self.show_archive_overwrite = false;
        self.show_update_restart = false;
        self.show_update_prompt = false;
        self.show_history_resume = false;
        self.show_profile = false;
        self.show_dest_picker = false;
    }

    /// Open archive confirmation dialog
    pub fn open_archive_confirm(&mut self, path: &str, kind: &str) {
        self.pending_archive_path = Some(path.to_string());
        self.pending_archive_kind = Some(kind.to_string());
        self.pending_archive_trim = true;
        self.show_archive_confirm = true;
    }

    /// Open destination picker for manage operations
    pub fn open_dest_picker(&mut self, action: ManageDestAction, src_path: String, src_name: String) {
        self.dest_action = Some(action);
        self.dest_source_path = Some(src_path);
        self.dest_source_name = Some(src_name);
        self.show_dest_picker = true;
    }

    /// Close destination picker
    pub fn close_dest_picker(&mut self) {
        self.show_dest_picker = false;
        self.dest_action = None;
        self.dest_source_path = None;
        self.dest_source_name = None;
    }
}
