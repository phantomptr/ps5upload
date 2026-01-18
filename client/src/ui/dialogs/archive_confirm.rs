/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Archive confirmation dialogs.

use crate::config::AppConfig;
use crate::i18n::{tr, Language};
use eframe::egui;

/// RAR extract mode label helper
pub fn rar_mode_label(lang: Language, mode: &str) -> String {
    match mode {
        "normal" => tr(lang, "archive_rar_mode_normal"),
        "safe" => tr(lang, "archive_rar_mode_safe"),
        "turbo" => tr(lang, "archive_rar_mode_turbo"),
        _ => tr(lang, "archive_rar_mode_normal"),
    }
}

/// Response from archive confirm dialog
pub enum ArchiveConfirmResponse {
    Continue { path: String, trim: bool },
    Cancel,
    None,
}

/// Render the archive confirmation dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    pending_path: &mut Option<String>,
    pending_kind: &mut Option<String>,
    pending_trim: &mut bool,
    custom_subfolder: &mut String,
    config: &mut AppConfig,
    connections: usize,
    lang: Language,
) -> ArchiveConfirmResponse {
    let mut response = ArchiveConfirmResponse::None;

    if !*show {
        return response;
    }

    let archive_kind = pending_kind.clone().unwrap_or_else(|| "Archive".to_string());

    egui::Window::new(tr(lang, "confirm_archive"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let detected_label = tr(lang, "archive_detected").replace("{}", &archive_kind);
            let is_rar = archive_kind.eq_ignore_ascii_case("RAR");
            ui.label(detected_label);

            if is_rar {
                ui.label(egui::RichText::new(tr(lang, "archive_rar_ps5_note")).weak());
                ui.horizontal(|ui| {
                    ui.label(tr(lang, "archive_rar_mode_label"));
                    let mut changed = false;
                    egui::ComboBox::from_id_source("rar_mode_confirm")
                        .selected_text(rar_mode_label(lang, &config.rar_extract_mode))
                        .show_ui(ui, |ui| {
                            changed |= ui
                                .selectable_value(
                                    &mut config.rar_extract_mode,
                                    "normal".to_string(),
                                    tr(lang, "archive_rar_mode_normal"),
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut config.rar_extract_mode,
                                    "safe".to_string(),
                                    tr(lang, "archive_rar_mode_safe"),
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut config.rar_extract_mode,
                                    "turbo".to_string(),
                                    tr(lang, "archive_rar_mode_turbo"),
                                )
                                .changed();
                        });
                    if changed {
                        let _ = config.save();
                    }
                });
                let note_key = match config.rar_extract_mode.as_str() {
                    "safe" => "archive_rar_safe_note",
                    "turbo" => "archive_rar_turbo_note",
                    _ => "archive_rar_normal_note",
                };
                ui.label(egui::RichText::new(tr(lang, note_key)).weak());
            } else {
                ui.label(egui::RichText::new(tr(lang, "archive_extract_stream_note")).weak());
            }

            if connections > 1 && !is_rar {
                ui.label(tr(lang, "archive_rar_stream_note"));
            }

            ui.checkbox(pending_trim, tr(lang, "archive_trim_dir"));
            if *pending_trim && !custom_subfolder.is_empty() {
                custom_subfolder.clear();
            }

            ui.horizontal(|ui| {
                if ui.button(tr(lang, "continue")).clicked() {
                    if let Some(path) = pending_path.take() {
                        response = ArchiveConfirmResponse::Continue {
                            path,
                            trim: *pending_trim,
                        };
                    }
                    *pending_kind = None;
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    *pending_path = None;
                    *pending_kind = None;
                    *pending_trim = true;
                    response = ArchiveConfirmResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}

/// Response from archive overwrite dialog
pub enum ArchiveOverwriteResponse {
    Continue { confirmed: bool },
    Cancel,
    None,
}

/// Render the archive overwrite confirmation dialog
pub fn render_overwrite(
    ctx: &egui::Context,
    show: &mut bool,
    confirmed: &mut bool,
    dest_path: &str,
    lang: Language,
) -> ArchiveOverwriteResponse {
    let mut response = ArchiveOverwriteResponse::None;

    if !*show {
        return response;
    }

    egui::Window::new(tr(lang, "confirm_overwrite"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Archive Extraction Warning\n\nThe destination folder already exists:\n{}\n\n\
                Extracting this archive will merge its contents with the existing folder. \
                Any files with the same name will be overwritten.",
                dest_path
            ));

            ui.checkbox(confirmed, "Allow overwriting of existing files");
            ui.label(
                egui::RichText::new(
                    "If unchecked, the upload will be cancelled to prevent data loss.",
                )
                .weak(),
            );

            ui.horizontal(|ui| {
                if ui.button(tr(lang, "continue")).clicked() {
                    response = ArchiveOverwriteResponse::Continue { confirmed: *confirmed };
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    response = ArchiveOverwriteResponse::Cancel;
                    *show = false;
                }
            });
        });

    response
}
