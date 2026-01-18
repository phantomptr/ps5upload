#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;
mod transfer;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::app_version,
            commands::config_load,
            commands::config_save,
            commands::storage_list,
            transfer::transfer_check_dest,
            transfer::transfer_scan,
            transfer::transfer_start,
            transfer::transfer_cancel
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
