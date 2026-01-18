/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Move confirmation dialog.

use crate::i18n::{tr, Language};
use crate::message::MoveRequest;
use eframe::egui;

/// Response from move confirmation dialog
pub enum MoveResponse {
    Move(MoveRequest),
    Cancel,
    None,
}

/// Render the move confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    request: Option<&MoveRequest>,
    lang: Language,
) -> MoveResponse {
    let mut response = MoveResponse::None;

    if !*show {
        return response;
    }

    let (dest_label, src_label, exists_label) = match request {
        Some(r) => (
            r.dst.clone(),
            r.src.clone(),
            if r.dst_exists {
                "Destination already exists."
            } else {
                "Destination does not exist yet."
            },
        ),
        None => (String::new(), String::new(), ""),
    };

    egui::Window::new(tr(lang, "confirm_move"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Move from:\n{}\n\nMove to:\n{}\n\n{}",
                src_label, dest_label, exists_label
            ));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "move")).clicked() {
                    if let Some(req) = request {
                        response = MoveResponse::Move(req.clone());
                    }
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = MoveResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
