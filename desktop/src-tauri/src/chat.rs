use serde::Serialize;
use std::sync::mpsc;
use std::thread;
use tauri::{AppHandle, Emitter, State};

use ps5upload_core::chat::{self, ChatMessage};
use ps5upload_core::chat_key::{chat_room_id_for_key, generate_chat_display_name, shared_key_hex};
use ps5upload_core::message::{AppMessage, ChatStatusEvent};

use crate::state::AppState;

#[derive(Clone, Serialize)]
struct ChatMessageEvent {
    time: String,
    sender: String,
    text: String,
    local: bool,
}

#[derive(Clone, Serialize)]
struct ChatStatusEventPayload {
    status: String,
}

#[derive(Clone, Serialize)]
struct ChatAckEventPayload {
    ok: bool,
    reason: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct ChatInfo {
    pub room_id: String,
    pub enabled: bool,
}

fn emit_message(handle: &AppHandle, msg: ChatMessage) {
    let _ = handle.emit(
        "chat_message",
        ChatMessageEvent {
            time: msg.time,
            sender: msg.sender,
            text: msg.text,
            local: msg.local,
        },
    );
}

fn emit_status(handle: &AppHandle, status: ChatStatusEvent) {
    let label = match status {
        ChatStatusEvent::Connected => "Connected",
        ChatStatusEvent::Disconnected => "Disconnected",
    };
    let _ = handle.emit(
        "chat_status",
        ChatStatusEventPayload {
            status: label.to_string(),
        },
    );
}

fn emit_ack(handle: &AppHandle, ok: bool, reason: Option<String>) {
    let _ = handle.emit(
        "chat_ack",
        ChatAckEventPayload { ok, reason },
    );
}

#[tauri::command]
pub fn chat_info() -> ChatInfo {
    let key = shared_key_hex();
    let trimmed = key.trim();
    ChatInfo {
        room_id: if trimmed.is_empty() {
            String::new()
        } else {
            chat_room_id_for_key(trimmed)
        },
        enabled: !trimmed.is_empty(),
    }
}

#[tauri::command]
pub fn chat_generate_name() -> String {
    generate_chat_display_name(shared_key_hex())
}

#[tauri::command]
pub fn chat_start(app_handle: AppHandle, state: State<AppState>) -> Result<ChatInfo, String> {
    let key = shared_key_hex().trim().to_string();
    if key.is_empty() {
        return Err("Chat key not configured.".to_string());
    }

    if state.chat_sender.lock().unwrap().is_some() {
        return Ok(ChatInfo {
            room_id: chat_room_id_for_key(&key),
            enabled: true,
        });
    }

    let (tx, rx) = mpsc::channel::<AppMessage>();
    let handle = tauri::async_runtime::handle().inner().clone();
    let sender = chat::start_chat_worker(tx, key.clone(), handle);

    let app = app_handle.clone();
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                AppMessage::ChatMessage(message) => emit_message(&app, message),
                AppMessage::ChatStatus(status) => emit_status(&app, status),
                AppMessage::ChatAck { ok, reason } => emit_ack(&app, ok, reason),
                _ => {}
            }
        }
    });

    *state.chat_sender.lock().unwrap() = Some(sender);

    Ok(ChatInfo {
        room_id: chat_room_id_for_key(&key),
        enabled: true,
    })
}

#[tauri::command]
pub fn chat_send(
    name: String,
    text: String,
    app_handle: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    let text_trimmed = text.trim().to_string();
    if text_trimmed.is_empty() {
        return Err("Message is empty.".to_string());
    }

    let payload = serde_json::json!({
        "name": name,
        "text": text_trimmed,
    })
    .to_string();

    emit_message(
        &app_handle,
        ChatMessage {
            time: chrono::Local::now().format("%H:%M").to_string(),
            sender: name.clone(),
            text: text_trimmed.clone(),
            local: true,
        },
    );

    let Some(sender) = state.chat_sender.lock().unwrap().clone() else {
        return Err("Chat is not connected.".to_string());
    };

    sender
        .send(payload)
        .map_err(|_| "Failed to send chat message.".to_string())?;

    Ok(())
}
