/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Resume confirmation dialog.

use crate::i18n::{tr, Language};
use eframe::egui;

/// Response from resume dialog
pub enum ResumeResponse {
    Resume,
    Overwrite,
    Cancel,
    None,
}

/// Render the resume confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    dest_path: &str,
    resume_mode: &str,
    lang: Language,
) -> ResumeResponse {
    let mut response = ResumeResponse::None;

    if !*show {
        return response;
    }

    egui::Window::new(tr(lang, "confirm_resume"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let mode_label = match resume_mode {
                "size" => tr(lang, "resume_fast"),
                "size_mtime" => tr(lang, "resume_medium"),
                "sha256" => tr(lang, "resume_slow"),
                _ => tr(lang, "resume"),
            };
            ui.label(format!(
                "Destination already exists:\n{}\n\n{}",
                dest_path, mode_label
            ));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "resume")).clicked() {
                    response = ResumeResponse::Resume;
                    *show = false;
                }
                if ui.button(tr(lang, "overwrite")).clicked() {
                    response = ResumeResponse::Overwrite;
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = ResumeResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
