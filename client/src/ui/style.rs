/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! UI styling and font configuration.

use crate::i18n::Language;
use eframe::egui;

/// Setup fonts with CJK and Arabic support based on language
pub fn setup_fonts(ctx: &egui::Context, lang: Language) {
    let mut fonts = egui::FontDefinitions::default();

    // Load Simplified Chinese font
    fonts.font_data.insert(
        "noto_sans_sc".to_owned(),
        egui::FontData::from_static(include_bytes!("../../fonts/NotoSansSC-Regular.otf")),
    );

    // Load Traditional Chinese font
    fonts.font_data.insert(
        "noto_sans_tc".to_owned(),
        egui::FontData::from_static(include_bytes!("../../fonts/NotoSansTC-Regular.otf")),
    );

    // Load Arabic font
    fonts.font_data.insert(
        "noto_sans_arabic".to_owned(),
        egui::FontData::from_static(include_bytes!("../../fonts/NotoSansArabic-Regular.ttf")),
    );

    // Add fonts as fallbacks for Proportional family (after default fonts)
    let proportional = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    if lang == Language::Ar {
        proportional.insert(0, "noto_sans_arabic".to_owned());
    }
    proportional.extend([
        "noto_sans_sc".to_owned(),
        "noto_sans_tc".to_owned(),
        "noto_sans_arabic".to_owned(),
    ]);
    let monospace = fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default();
    if lang == Language::Ar {
        monospace.insert(0, "noto_sans_arabic".to_owned());
    } else {
        monospace.push("noto_sans_arabic".to_owned());
    }

    ctx.set_fonts(fonts);
}

/// Configure common style settings (spacing, text sizes)
fn apply_common_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(22.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(13.0, egui::FontFamily::Monospace),
    );
    style.spacing.item_spacing = [4.0, 4.0].into();
    style.spacing.button_padding = [8.0, 5.0].into();
    style.spacing.window_margin = egui::Margin::same(4.0);
    style.spacing.menu_margin = egui::Margin::same(4.0);
    style.spacing.indent = 14.0;
    ctx.set_style(style);
}

/// Apply dark theme
pub fn setup_dark_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    let accent = egui::Color32::from_rgb(0, 164, 230);
    let accent_soft = egui::Color32::from_rgb(20, 70, 110);
    visuals.window_rounding = 10.0.into();
    visuals.menu_rounding = 8.0.into();
    visuals.window_fill = egui::Color32::from_rgb(18, 22, 30);
    visuals.panel_fill = egui::Color32::from_rgb(14, 18, 26);
    visuals.faint_bg_color = egui::Color32::from_rgb(20, 25, 35);
    visuals.extreme_bg_color = egui::Color32::from_rgb(10, 13, 19);
    visuals.code_bg_color = egui::Color32::from_rgb(28, 36, 50);
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(240, 248, 255));
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(24, 30, 44);
    visuals.widgets.inactive.bg_stroke.color = egui::Color32::from_rgb(40, 52, 70);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(220, 230, 240);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(30, 38, 54);
    visuals.widgets.hovered.bg_stroke.color = accent_soft;
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(18, 54, 84);
    visuals.widgets.active.bg_stroke.color = accent;
    visuals.widgets.noninteractive.bg_stroke.color = egui::Color32::from_rgb(38, 46, 62);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(190, 200, 214);
    visuals.override_text_color = Some(egui::Color32::from_rgb(230, 236, 245));
    visuals.hyperlink_color = egui::Color32::from_rgb(115, 200, 255);
    ctx.set_visuals(visuals);
    apply_common_style(ctx);
}

/// Apply light theme
pub fn setup_light_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    let accent = egui::Color32::from_rgb(0, 120, 196);
    let accent_soft = egui::Color32::from_rgb(180, 215, 245);
    visuals.window_rounding = 10.0.into();
    visuals.menu_rounding = 8.0.into();
    visuals.window_fill = egui::Color32::from_rgb(250, 252, 255);
    visuals.panel_fill = egui::Color32::from_rgb(244, 247, 252);
    visuals.faint_bg_color = egui::Color32::from_rgb(235, 239, 245);
    visuals.extreme_bg_color = egui::Color32::from_rgb(230, 235, 242);
    visuals.code_bg_color = egui::Color32::from_rgb(225, 232, 242);
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 255, 255));
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(236, 240, 246);
    visuals.widgets.inactive.bg_stroke.color = egui::Color32::from_rgb(200, 210, 224);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(40, 50, 64);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(228, 235, 245);
    visuals.widgets.hovered.bg_stroke.color = accent_soft;
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(208, 230, 248);
    visuals.widgets.active.bg_stroke.color = accent;
    visuals.widgets.noninteractive.bg_stroke.color = egui::Color32::from_rgb(190, 200, 214);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(30, 40, 55);
    visuals.override_text_color = Some(egui::Color32::from_rgb(20, 28, 40));
    visuals.hyperlink_color = egui::Color32::from_rgb(0, 98, 180);
    ctx.set_visuals(visuals);
    apply_common_style(ctx);
}

/// Note text color for dark theme
pub const NOTE_COLOR_DARK: egui::Color32 = egui::Color32::from_rgb(245, 210, 120);

/// Note text color for light theme
pub const NOTE_COLOR_LIGHT: egui::Color32 = egui::Color32::from_rgb(120, 80, 10);
