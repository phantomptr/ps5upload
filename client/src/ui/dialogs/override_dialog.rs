/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Override confirmation dialog.

use crate::i18n::{tr, Language};
use eframe::egui;

/// Response from override dialog
pub enum OverrideResponse {
    Overwrite,
    Cancel,
    None,
}

/// Render the override confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    dest_path: &str,
    lang: Language,
) -> OverrideResponse {
    let mut response = OverrideResponse::None;

    if !*show {
        return response;
    }

    egui::Window::new(tr(lang, "confirm_overwrite"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Folder already exists:\n{}\n\nOverwrite it?",
                dest_path
            ));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "overwrite")).clicked() {
                    response = OverrideResponse::Overwrite;
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = OverrideResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
