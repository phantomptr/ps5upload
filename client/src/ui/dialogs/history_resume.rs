/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! History resume dialog.

use crate::history::TransferRecord;
use crate::i18n::{tr, Language};
use crate::ui::note_text;
use eframe::egui;

/// Response from history resume dialog
pub enum HistoryResumeResponse {
    Resume { record: TransferRecord, mode: String },
    Cancel,
    None,
}

/// Render the history resume dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    pending_record: &Option<TransferRecord>,
    resume_mode: &mut String,
    lang: Language,
    theme_dark: bool,
) -> HistoryResumeResponse {
    let mut response = HistoryResumeResponse::None;

    if !*show {
        return response;
    }

    let dest_label = pending_record
        .as_ref()
        .map(|r| r.dest_path.clone())
        .unwrap_or_default();
    let src_label = pending_record
        .as_ref()
        .map(|r| r.source_path.clone())
        .unwrap_or_default();

    egui::Window::new(tr(lang, "resume_title"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "{}:\n{}\n\n{}:\n{}",
                tr(lang, "source_folder"),
                src_label,
                tr(lang, "destination"),
                dest_label
            ));

            ui.horizontal(|ui| {
                ui.label(tr(lang, "resume_mode"));
                egui::ComboBox::from_id_source("history_resume_mode_combo")
                    .selected_text(match resume_mode.as_str() {
                        "size" => tr(lang, "resume_fast"),
                        "size_mtime" => tr(lang, "resume_medium"),
                        "sha256" => tr(lang, "resume_slow"),
                        _ => tr(lang, "off"),
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(*resume_mode == "size", tr(lang, "resume_fast"))
                            .clicked()
                        {
                            *resume_mode = "size".to_string();
                        }
                        if ui
                            .selectable_label(*resume_mode == "size_mtime", tr(lang, "resume_medium"))
                            .clicked()
                        {
                            *resume_mode = "size_mtime".to_string();
                        }
                        if ui
                            .selectable_label(*resume_mode == "sha256", tr(lang, "resume_slow"))
                            .clicked()
                        {
                            *resume_mode = "sha256".to_string();
                        }
                        if ui
                            .selectable_label(*resume_mode == "none", tr(lang, "off"))
                            .clicked()
                        {
                            *resume_mode = "none".to_string();
                        }
                    });
            });

            let note = tr(lang, "note_resume");
            ui.label(note_text(&note, theme_dark));

            ui.horizontal(|ui| {
                if ui.button(tr(lang, "resume")).clicked() {
                    if let Some(record) = pending_record.clone() {
                        response = HistoryResumeResponse::Resume {
                            record,
                            mode: resume_mode.clone(),
                        };
                    }
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = HistoryResumeResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
