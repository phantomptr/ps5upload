#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod chat;
mod connection;
mod logging;
mod manage;
mod meta;
mod payload;
mod paths;
mod state;
mod transfer;
mod update;


fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state::AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            payload::start_payload_poller(handle.clone());
            payload::start_payload_auto_reloader(handle.clone());
            connection::start_connection_poller(handle.clone());
            manage::start_manage_poller(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_version,
            commands::config_load,
            commands::config_save,
            commands::config_update,
            commands::profiles_load,
            commands::profiles_save,
            commands::profiles_update,
            commands::queue_load,
            commands::queue_save,
            commands::queue_update,
            commands::history_load,
            commands::history_add,
            commands::history_clear,
            commands::set_save_logs,
            commands::set_ui_log_enabled,
            commands::storage_list,
            commands::port_check,
            connection::connection_set_ip,
            connection::connection_polling_set,
            connection::connection_auto_set,
            connection::connection_snapshot,
            connection::connection_connect,
            manage::manage_list,
            manage::manage_list_snapshot,
            manage::manage_list_refresh,
            manage::manage_polling_set,
            manage::manage_set_ip,
            manage::manage_set_path,
            manage::manage_cancel,
            manage::manage_delete,
            manage::manage_rename,
            manage::manage_create_dir,
            manage::manage_chmod,
            manage::manage_move,
            manage::manage_copy,
            manage::manage_extract,
            manage::manage_download_file,
            manage::manage_download_dir,
            manage::manage_upload,
            payload::payload_send,
            payload::payload_download_and_send,
            payload::payload_check,
            payload::payload_probe,
            payload::payload_status,
            payload::payload_status_snapshot,
            payload::payload_status_refresh,
            payload::payload_polling_set,
            payload::payload_set_ip,
            payload::payload_auto_reload_set,
            payload::payload_queue_extract,
            payload::payload_queue_cancel,
            payload::payload_queue_clear,
            update::update_check,
            update::update_check_tag,
            update::update_download_asset,
            update::update_current_asset_name,
            update::update_prepare_self,
            update::update_apply_self,
            meta::game_meta_load,
            meta::manage_rar_metadata,
            chat::chat_info,
            chat::chat_generate_name,
            chat::chat_start,
            chat::chat_send,
            transfer::transfer_check_dest,
            transfer::transfer_scan,
            transfer::transfer_start,
            transfer::transfer_cancel,
            transfer::transfer_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
