/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Delete confirmation dialog.

use crate::i18n::{tr, Language};
use eframe::egui;

/// Response from delete confirmation dialog
pub enum DeleteResponse {
    Delete,
    Cancel,
    None,
}

/// Render the delete confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    target: Option<&str>,
    lang: Language,
) -> DeleteResponse {
    let mut response = DeleteResponse::None;

    if !*show {
        return response;
    }

    let target_path = target.unwrap_or_default();

    egui::Window::new(tr(lang, "confirm_delete"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("Delete:\n{}\n\nThis cannot be undone.", target_path));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "delete_action")).clicked() {
                    response = DeleteResponse::Delete;
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = DeleteResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
