/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Profile management dialog.

use crate::i18n::{tr, Language};
use crate::profiles::{Profile, ProfilesData};
use crate::ui::{row_align, text_edit};
use eframe::egui;

/// Actions from profile manager
pub enum ProfileAction {
    Delete(String),
    Load(Profile),
    Save(String),
    Close,
    None,
}

/// Render the profile manager dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    profiles_data: &ProfilesData,
    name_input: &mut String,
    lang: Language,
    rtl: bool,
) -> ProfileAction {
    let mut action = ProfileAction::None;

    if !*show {
        return action;
    }

    egui::Window::new("Manage Profiles")
        .collapsible(false)
        .resizable(true)
        .default_size([350.0, 300.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.heading("Saved Profiles");

            let mut profile_to_delete: Option<String> = None;
            let mut profile_to_apply: Option<Profile> = None;

            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    if profiles_data.profiles.is_empty() {
                        ui.label("No profiles saved yet.");
                    } else {
                        for profile in &profiles_data.profiles {
                            ui.horizontal(|ui| {
                                ui.label(&profile.name);
                                row_align(ui, true, |ui| {
                                    if ui.small_button("Delete").clicked() {
                                        profile_to_delete = Some(profile.name.clone());
                                    }
                                    if ui.small_button("Load").clicked() {
                                        profile_to_apply = Some(profile.clone());
                                    }
                                });
                            });
                            ui.separator();
                        }
                    }
                });

            if let Some(name) = profile_to_delete {
                action = ProfileAction::Delete(name);
            }
            if let Some(profile) = profile_to_apply {
                action = ProfileAction::Load(profile);
            }

            ui.separator();

            ui.heading(tr(lang, "save_current_settings"));
            ui.horizontal(|ui| {
                ui.label(tr(lang, "profile_name"));
                ui.add(text_edit(name_input, rtl));
            });

            ui.horizontal(|ui| {
                let can_save = !name_input.trim().is_empty();
                if ui
                    .add_enabled(can_save, egui::Button::new(tr(lang, "save_profile")))
                    .clicked()
                {
                    let name = name_input.trim().to_string();
                    action = ProfileAction::Save(name);
                    name_input.clear();
                }
                if ui.button(tr(lang, "close")).clicked() {
                    action = ProfileAction::Close;
                    *show = false;
                }
            });
        });

    action
}
