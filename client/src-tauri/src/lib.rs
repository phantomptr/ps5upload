//! PS5 Upload — Tauri desktop shell (Rust backend).
//!
//! The renderer (React + Vite) calls into Rust via `tauri::invoke`
//! against the `#[command]` handlers in `commands`. Engine lifecycle,
//! file dialogs, and app-data persistence live on this side.
//!
//! Architecture:
//!
//!   Tauri main (this crate) ── spawns ──> ps5upload-engine (Rust sibling)
//!        │                                       │
//!        ├ tauri::invoke commands                │ HTTP :19113
//!        │   (fs, config, dialogs, status)       │
//!        │                                       ▼
//!        └ WebView (React renderer) ── fetch ──> engine HTTP API
//!
//! We keep the engine as its own process so its HTTP surface stays a
//! stable, language-agnostic interface. A later phase can link
//! `ps5upload-core` directly and remove the HTTP hop entirely;
//! nothing in the command contract needs to change for that.

mod commands;
mod engine;

/// Build and run the Tauri application. `main.rs` just calls this.
pub fn run() {
    let app = tauri::Builder::default()
        // Single-instance MUST be the first plugin so its `init` runs
        // before any other setup (window creation, engine spawn). When
        // a second launch hits, the runtime calls our callback in the
        // ORIGINAL process with the second instance's argv, then exits
        // the duplicate. This prevents the double-orphan-reap race
        // where launch #2's `engine::start()` would otherwise kill
        // launch #1's engine and leave its UI talking to a dead port.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Focus the existing main window so the user sees something
            // happen rather than a silent no-op when they re-launch.
            // `get_webview_window("main")` matches the window label
            // declared in tauri.conf.json.
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Spawn the Rust engine binary as a sidecar. On failure we log and
            // keep the window open so the user can see diagnostic info — the
            // UI's passive status polling will flag the unreachable engine
            // without requiring a re-launch.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match engine::start(&handle).await {
                    Ok(url) => eprintln!("[tauri] engine ready at {url}"),
                    Err(e) => eprintln!("[tauri] engine failed to start: {e}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ── Engine proxies ──────────────────────────────────────
            // Thin wrappers over the ps5upload-engine HTTP API
            // (localhost:19113). Used by the Upload/Library/Volumes/
            // FileSystem/Hardware surfaces.
            commands::ps5_volumes,
            commands::ps5_list_dir,
            commands::transfer_file,
            commands::transfer_dir,
            commands::transfer_dir_reconcile,
            commands::transfer_download,
            commands::ps5_fs_delete,
            commands::ps5_fs_move,
            commands::ps5_fs_copy,
            commands::ps5_fs_op_status,
            commands::ps5_fs_op_cancel,
            commands::ps5_fs_mount,
            commands::ps5_fs_unmount,
            commands::ps5_fs_chmod,
            commands::ps5_fs_mkdir,
            commands::ps5_hw_info,
            commands::ps5_hw_temps,
            commands::ps5_hw_power,
            commands::ps5_hw_set_fan_threshold,
            commands::job_status,
            commands::engine_logs_tail,
            // ── Scene-tool integration ──────────────────────────────
            // `companion_probe` checks which well-known scene tools
            // are alive on the PS5 host.
            commands::companion_probe,
            // ── Persistence (send-payload history) ──────────────────
            commands::send_payload_history_load,
            commands::send_payload_history_add,
            commands::send_payload_history_clear,
            // ── Persistence (cross-session resume tx_ids) ───────────
            // The client generates a stable tx_id for each upload and
            // remembers it here; on Resume after a failed run (or even
            // after closing the app), the same tx_id is reused so the
            // payload's journal can surface last_acked_shard.
            commands::resume_txid_lookup,
            commands::resume_txid_remember,
            commands::resume_txid_forget,
            // ── Persistence (upload queue + payload playlists) ──────
            // Whole-document JSON stores. Renderer owns the shape.
            commands::upload_queue_load,
            commands::upload_queue_save,
            commands::payload_playlists_load,
            commands::payload_playlists_save,
            // ── FAQ + changelog content (bundled markdown) ──────────
            commands::faq_load,
            commands::changelog_load,
            // ── Host-local helpers (in-process, no HTTP hop) ────────
            commands::inspect_folder, // param.json + disk-footprint walk
            commands::path_kind,      // "file" | "folder" for drag-drop routing
            commands::payload_bundled_path, // resolve the bundled ps5upload.elf
            commands::keep_awake_set, // spawn/kill platform sleep inhibitor
            commands::keep_awake_state,
            commands::user_config_load, // ~/.ps5upload/settings.json read
            commands::user_config_save, // ~/.ps5upload/settings.json atomic write
            commands::user_config_path_resolved, // show-the-path for the Settings UI
            // ── Connectivity + payload lifecycle probes ─────────────
            // Used by the Connection + Send payload tabs for
            // reachability checks and payload delivery.
            commands::port_check,
            commands::payload_send,
            commands::payload_check,
            commands::payload_probe,
            // ── Self-update ─────────────────────────────────────────
            // Check the GitHub release manifest, download the
            // platform-specific archive, reveal it so the user can
            // install manually. No installer / no signing needed. See
            // commands/updates.rs for the full flow.
            commands::update_check,
            commands::update_download,
        ])
        .build(tauri::generate_context!())
        .expect("tauri runtime failed to build");

    // App-level run loop. We use `RunEvent::Exit` (fires once when the
    // Tauri runtime is about to leave its run loop, regardless of how
    // many windows are open / closed) instead of the previous
    // per-window `CloseRequested` so multi-window scenarios don't kill
    // the engine on first-window-close, and so the kill always runs
    // before the process exits. Combined with the engine's stdin-EOF
    // parent watcher (which catches the case where this process dies
    // without running to here at all — segfault, taskkill /F, OOM,
    // etc.), the engine never gets orphaned holding 19113.
    app.run(|_handle, event| {
        if let tauri::RunEvent::Exit = event {
            // block_on is fine here: we're on the main thread, the
            // tauri runtime has already finished its event loop, and
            // the kill is a couple of syscalls + a short wait. The
            // tokio runtime stays alive until we return from this
            // closure, so the async kill path completes.
            tauri::async_runtime::block_on(async {
                engine::stop().await;
            });
        }
    });
}
