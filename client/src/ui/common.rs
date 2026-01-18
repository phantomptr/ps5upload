/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Common UI utilities and shared components.

use crate::i18n::Language;
use crate::ui::style::{NOTE_COLOR_DARK, NOTE_COLOR_LIGHT};
use eframe::egui;

/// Check if language is right-to-left
pub fn is_rtl(lang: Language) -> bool {
    matches!(lang, Language::Ar)
}

/// Create a horizontal row with proper RTL support
pub fn row(ui: &mut egui::Ui, rtl: bool, add_contents: impl FnOnce(&mut egui::Ui)) {
    if rtl {
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            add_contents,
        );
    } else {
        ui.with_layout(
            egui::Layout::left_to_right(egui::Align::Center),
            add_contents,
        );
    }
}

/// Create a row with optional right alignment
pub fn row_align(ui: &mut egui::Ui, align_right: bool, add_contents: impl FnOnce(&mut egui::Ui)) {
    let mut layout = egui::Layout::left_to_right(egui::Align::Center);
    if align_right {
        layout = layout.with_main_align(egui::Align::Max);
    }
    ui.with_layout(layout, add_contents);
}

/// Create a single-line text edit with RTL support
pub fn text_edit<'a>(value: &'a mut String, rtl: bool) -> egui::TextEdit<'a> {
    let edit = egui::TextEdit::singleline(value);
    if rtl {
        edit.horizontal_align(egui::Align::RIGHT)
    } else {
        edit
    }
}

/// Create styled note text
pub fn note_text(text: &str, theme_dark: bool) -> egui::RichText {
    let color = if theme_dark {
        NOTE_COLOR_DARK
    } else {
        NOTE_COLOR_LIGHT
    };
    egui::RichText::new(text).color(color).italics()
}

/// Format bytes into human-readable string
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Format duration in seconds to human-readable string
pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "N/A".to_string();
    }
    let total = seconds.round() as u64;
    let mins = total / 60;
    let secs = total % 60;
    if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Format modification time to human-readable string
pub fn format_modified_time(mtime: Option<i64>) -> String {
    let Some(ts) = mtime else {
        return "--".to_string();
    };

    let now = chrono::Utc::now().timestamp();
    let delta = now.saturating_sub(ts);
    if delta < 60 {
        return "just now".to_string();
    }
    if delta < 3600 {
        return format!("{}m ago", delta / 60);
    }
    if delta < 86400 {
        return format!("{}h ago", delta / 3600);
    }
    if delta < 86400 * 7 {
        return format!("{}d ago", delta / 86400);
    }

    let Some(dt_utc) = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0) else {
        return "--".to_string();
    };
    let dt_local = dt_utc.with_timezone(&chrono::Local);
    dt_local.format("%Y-%m-%d %H:%M").to_string()
}

/// Join remote paths correctly
pub fn join_remote_path(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{}{}", base, name)
    } else {
        format!("{}/{}", base, name)
    }
}
