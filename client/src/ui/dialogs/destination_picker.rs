/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Destination picker dialog for move/copy/extract operations.

use crate::i18n::{tr, Language};
use crate::protocol::DirEntry;
use crate::ui::{join_remote_path, text_edit, ManageDestAction};
use eframe::egui;

/// Actions from destination picker
pub enum DestPickerAction {
    GoToPath(String),
    Up,
    Refresh,
    Select { action: ManageDestAction, src: String, dst: String },
    Cancel,
    None,
}

/// Render the destination picker dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    path: &mut String,
    entries: &[DirEntry],
    selected: &mut Option<usize>,
    status: &str,
    action: Option<ManageDestAction>,
    source_path: Option<&str>,
    source_name: Option<&str>,
    lang: Language,
    rtl: bool,
) -> DestPickerAction {
    let mut result = DestPickerAction::None;

    if !*show {
        return result;
    }

    egui::Window::new(tr(lang, "select_destination"))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.heading(tr(lang, "select_destination"));
            ui.label(tr(lang, "destination_path"));

            ui.horizontal(|ui| {
                let buttons_width = 3.0 * 70.0 + 12.0;
                let path_width = (ui.available_width() - buttons_width).max(200.0);
                ui.add_sized([path_width, 24.0], text_edit(path, rtl));
                if ui.button(tr(lang, "go")).clicked() {
                    result = DestPickerAction::GoToPath(path.clone());
                }
                if ui.button(tr(lang, "up")).clicked() {
                    result = DestPickerAction::Up;
                }
                if ui.button(tr(lang, "refresh")).clicked() {
                    result = DestPickerAction::Refresh;
                }
            });

            // Breadcrumb navigation
            ui.horizontal_wrapped(|ui| {
                ui.label(tr(lang, "breadcrumb"));
                let mut parts = Vec::new();
                let mut current = String::new();
                let active_path = path.as_str();
                if active_path.starts_with('/') {
                    parts.push("/".to_string());
                }
                for part in active_path.split('/').filter(|p| !p.is_empty()) {
                    if current.ends_with('/') || current.is_empty() {
                        current.push_str(part);
                    } else {
                        current.push('/');
                        current.push_str(part);
                    }
                    parts.push(current.clone());
                }

                for (idx, part) in parts.iter().enumerate() {
                    let label = if idx == 0 && part == "/" {
                        "/"
                    } else {
                        part.rsplit('/').next().unwrap_or(part)
                    };
                    if ui.button(label).clicked() {
                        *path = part.clone();
                        result = DestPickerAction::GoToPath(part.clone());
                    }
                }
            });

            ui.label(status);

            let mut open_dir: Option<String> = None;
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (idx, entry) in entries.iter().enumerate() {
                        if entry.entry_type != "dir" {
                            continue;
                        }
                        let is_selected = *selected == Some(idx);
                        let response = ui.add(egui::SelectableLabel::new(
                            is_selected,
                            format!("📁 {}", entry.name),
                        ));
                        if response.clicked() {
                            *selected = Some(idx);
                        }
                        if response.double_clicked() {
                            open_dir = Some(entry.name.clone());
                        }
                    }
                });

            if let Some(dir_name) = open_dir {
                *path = join_remote_path(path, &dir_name);
                result = DestPickerAction::GoToPath(path.clone());
            }

            let selected_dest = selected
                .and_then(|idx| entries.get(idx))
                .filter(|e| e.entry_type == "dir")
                .map(|e| join_remote_path(path, &e.name))
                .unwrap_or_else(|| path.clone());

            ui.label(format!("{} {}", tr(lang, "destination"), selected_dest));

            ui.horizontal(|ui| {
                if ui.button(tr(lang, "select_here")).clicked() {
                    if let (Some(act), Some(src), Some(name)) = (action, source_path, source_name) {
                        let dst = join_remote_path(&selected_dest, name);
                        result = DestPickerAction::Select {
                            action: act,
                            src: src.to_string(),
                            dst,
                        };
                    }
                    *show = false;
                }
                if ui.button(tr(lang, "cancel")).clicked() {
                    result = DestPickerAction::Cancel;
                    *show = false;
                }
            });
        });

    result
}
