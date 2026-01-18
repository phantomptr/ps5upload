/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Rename confirmation dialog.

use crate::i18n::{tr, Language};
use crate::message::RenameRequest;
use eframe::egui;

/// Response from rename confirmation dialog
pub enum RenameResponse {
    Rename(RenameRequest),
    Cancel,
    None,
}

/// Render the rename confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    request: Option<&RenameRequest>,
    lang: Language,
) -> RenameResponse {
    let mut response = RenameResponse::None;

    if !*show {
        return response;
    }

    let (src_label, dst_label) = match request {
        Some(r) => (r.src.clone(), r.dst.clone()),
        None => (String::new(), String::new()),
    };

    egui::Window::new(tr(lang, "confirm_rename"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Rename from:\n{}\n\nRename to:\n{}",
                src_label, dst_label
            ));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "rename")).clicked() {
                    if let Some(req) = request {
                        response = RenameResponse::Rename(req.clone());
                    }
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = RenameResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
