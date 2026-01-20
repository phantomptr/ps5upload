/*
 * PS5 Upload - Fast App Transfer for PS5
 * Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

//! Core library exports for non-UI consumers (e.g., Tauri wrapper).

pub mod archive;
pub mod chat;
pub mod chat_key;
pub mod config;
pub mod game_meta;
pub mod history;
pub mod i18n;
pub mod message;
pub mod paths;
pub mod profiles;
pub mod protocol;
pub mod queue;
pub mod transfer;
pub mod transfer_utils;
pub mod update;
pub mod unrar;

pub use message::{AppMessage, ChatStatusEvent};
