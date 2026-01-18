/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Update prompt dialogs.

use crate::i18n::{tr, Language};
use crate::message::ReleaseInfo;
use crate::ui::row;
use eframe::egui;

/// Actions from update prompt dialog
pub enum UpdatePromptAction {
    OpenReleasePage(String),
    UpdateNow,
    Later,
    None,
}

/// Render the update prompt dialog
pub fn render(
    ctx: &egui::Context,
    show: &mut bool,
    info: Option<&ReleaseInfo>,
    current_version: &str,
    has_pending_update: bool,
    lang: Language,
    rtl: bool,
) -> UpdatePromptAction {
    let mut action = UpdatePromptAction::None;

    if !*show {
        return action;
    }

    let Some(info) = info else {
        *show = false;
        return action;
    };

    egui::Window::new(tr(lang, "update_prompt_title"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let latest = &info.tag_name;
            let body = tr(lang, "update_prompt_body")
                .replacen("{}", latest, 1)
                .replacen("{}", current_version, 1);
            ui.label(body);

            row(ui, rtl, |ui| {
                if ui.button(tr(lang, "open_release_page")).clicked() {
                    action = UpdatePromptAction::OpenReleasePage(info.html_url.clone());
                }
            });

            ui.horizontal(|ui| {
                if ui.button(tr(lang, "update_now")).clicked() {
                    *show = false;
                    action = if has_pending_update {
                        UpdatePromptAction::UpdateNow
                    } else {
                        UpdatePromptAction::UpdateNow
                    };
                }
                if ui.button(tr(lang, "update_prompt_later")).clicked() {
                    *show = false;
                    action = UpdatePromptAction::Later;
                }
            });
        });

    action
}

/// Actions from update restart dialog
pub enum UpdateRestartAction {
    RestartNow,
    Later,
    None,
}

/// Render the update restart dialog
pub fn render_restart(
    ctx: &egui::Context,
    show: &mut bool,
    lang: Language,
) -> UpdateRestartAction {
    let mut action = UpdateRestartAction::None;

    if !*show {
        return action;
    }

    egui::Window::new(tr(lang, "update_restart_title"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tr(lang, "update_restart_note"));
            ui.horizontal(|ui| {
                if ui.button(tr(lang, "update_restart_now")).clicked() {
                    *show = false;
                    action = UpdateRestartAction::RestartNow;
                }
                if ui.button(tr(lang, "update_restart_later")).clicked() {
                    *show = false;
                    action = UpdateRestartAction::Later;
                }
            });
        });

    action
}
