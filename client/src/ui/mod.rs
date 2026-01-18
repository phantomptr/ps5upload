/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! UI module containing all user interface components.

pub mod common;
pub mod header;
pub mod status_bar;
pub mod style;

// Re-export commonly used items
pub use common::{
    format_bytes,
    format_duration,
    format_modified_time,
    is_rtl,
    join_remote_path,
    note_text,
    row,
    row_align,
    text_edit,
};
pub use style::{setup_dark_style, setup_fonts, setup_light_style};
