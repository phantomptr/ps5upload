/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Top header panel with logo, title, and theme toggle.

use crate::ui::row_align;
use eframe::egui;

/// Render response for header panel
pub struct HeaderResponse {
    pub coffee_clicked: bool,
    pub theme_toggle_clicked: bool,
}

/// Render the top header panel
pub fn render(
    ctx: &egui::Context,
    logo_texture: Option<&egui::TextureHandle>,
    theme_dark: bool,
) -> HeaderResponse {
    let mut response = HeaderResponse {
        coffee_clicked: false,
        theme_toggle_clicked: false,
    };

    egui::TopBottomPanel::top("header")
        .exact_height(56.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.horizontal(|ui| {
                    if let Some(tex) = logo_texture {
                        ui.add(egui::Image::new(tex).fit_to_exact_size([48.0, 48.0].into()));
                    }
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("PS5Upload").strong().size(22.0));
                        ui.label(
                            egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .size(14.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
                row_align(ui, true, |ui| {
                    if ui
                        .add(
                            egui::Button::new("☕ Buy me a coffee")
                                .min_size([120.0, 32.0].into()),
                        )
                        .clicked()
                    {
                        response.coffee_clicked = true;
                    }
                    ui.separator();
                    let theme_icon = if theme_dark { "🌙" } else { "☀" };
                    if ui
                        .button(theme_icon)
                        .on_hover_text("Toggle Theme")
                        .clicked()
                    {
                        response.theme_toggle_clicked = true;
                    }
                });
            });
        });

    response
}
