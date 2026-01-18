/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Download confirmation dialog.

use crate::i18n::{tr, Language};
use crate::ui::dialogs::DownloadRequest;
use eframe::egui;

/// Response from download confirmation dialog
pub enum DownloadResponse {
    Download(DownloadRequest),
    Cancel,
    None,
}

/// Render the download confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    request: Option<&DownloadRequest>,
    lang: Language,
) -> DownloadResponse {
    let mut response = DownloadResponse::None;

    if !*show {
        return response;
    }

    let dest_label = match request {
        Some(DownloadRequest::File { save_path, .. }) => save_path.clone(),
        Some(DownloadRequest::Dir { dest_root, .. }) => dest_root.clone(),
        None => String::new(),
    };

    egui::Window::new(tr(lang, "confirm_download"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Destination already exists:\n{}\n\nOverwrite and download?",
                dest_label
            ));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "download")).clicked() {
                    if let Some(req) = request {
                        response = DownloadResponse::Download(req.clone());
                    }
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = DownloadResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
