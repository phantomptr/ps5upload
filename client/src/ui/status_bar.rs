/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Bottom status bar with status text and credits.

use crate::ui::row_align;
use eframe::egui;

/// Render the bottom status bar
pub fn render(ctx: &egui::Context, status: &str, rtl: bool) {
    egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
        let mut status_layout = egui::Layout::left_to_right(egui::Align::Center);
        if rtl {
            status_layout = status_layout.with_main_align(egui::Align::Max);
        }
        ui.with_layout(status_layout, |ui| {
            ui.label(egui::RichText::new(status).strong());
            row_align(ui, !rtl, |ui| {
                ui.hyperlink_to("Created by PhantomPtr", "https://x.com/phantomptr");
                ui.label("|");
                ui.hyperlink_to("Source Code", "https://github.com/phantomptr/ps5upload");
            });
        });
    });
}
