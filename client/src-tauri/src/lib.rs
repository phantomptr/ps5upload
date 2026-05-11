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
            commands::transfer_dir_diff_preview,
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
            commands::ps5_hw_storage,
            commands::ps5_hw_set_fan_threshold,
            commands::ps5_app_launch,
            commands::ps5_app_register,
            commands::ps5_app_unregister,
            commands::job_status,
            commands::engine_logs_tail,
            // ── PKG install (parse + host + install via BGFT) ─────────
            // Engine proxies for the new Install Package tab. Single
            // path: parse local .pkg → host over HTTP → tell payload to
            // call sceBgftService* → poll status.
            commands::pkg_metadata,
            commands::pkg_metadata_split,
            // Read-only UFS2 image inspector (browse a local .ffpkg
            // before uploading). See ps5upload_pkg::ufs2.
            commands::ffpkg_inspect,
            commands::ffpkg_extract,
            commands::pkg_install_start,
            commands::pkg_install_status,
            commands::pkg_install_cancel,
            // ── Scene-tool integration ──────────────────────────────
            // `companion_probe` checks which well-known scene tools
            // are alive on the PS5 host.
            commands::companion_probe,
            // ── LAN discovery (mDNS-SD + TCP probe) ─────────────────
            // `discover_ps5` browses well-known mDNS service types
            // and TCP-probes :9021/:9114 on each discovered host so
            // the Connection screen's "Find PS5s" button can populate
            // the IP field automatically. Read-only (we don't
            // advertise ourselves). See commands/discover.rs.
            commands::discover_ps5,
            // ── Payload Library (curated catalogue + GitHub fetch) ──
            // The Payloads tab uses these to list third-party
            // homebrew payloads (kstuff, SMP, etaHEN, …),
            // fetch their latest releases from GitHub, cache to disk,
            // and re-send via the existing payload_send flow. See
            // commands/payloads.rs.
            commands::payloads_catalog,
            commands::payloads_release,
            commands::payloads_local_inventory,
            commands::payloads_local_path,
            commands::payloads_download,
            // ── ShadowMount+ awareness (read-only) ──────────────────
            // `smp_status` collects the on-console SMP state — config,
            // autotune, mounted images, debug-log tail — so the
            // Library tab can surface it without making the user FTP
            // into the console. We never write SMP's files. See
            // commands/smp.rs.
            commands::smp_status,
            // ── USB autoloader wizard ───────────────────────────────
            // Enumerate removable drives + write a curated
            // ps5_autoloader/ folder to one. Cross-platform; pulls
            // payloads from the Phase 2 cache, never re-downloads
            // here. See commands/usb_autoloader.rs.
            commands::usb_list_removable,
            commands::usb_autoloader_install,
            // ── Game metadata healing ───────────────────────────────
            // `heal_appmeta` restores missing /user/appmeta/<TID>/
            // entries (icon0.png, param.json, snd0.at9) from the
            // game source's sce_sys/. See commands/heal_appmeta.rs.
            commands::heal_appmeta,
            // ── PS5 power control (reboot/shutdown/standby/tick) ────
            // Sends SYSTEM_CONTROL frames; the destructive actions
            // tear down the network so a connection-drop is treated
            // as success in core. See commands/system_power.rs.
            commands::power_reboot,
            commands::power_shutdown,
            commands::power_standby,
            commands::power_tick,
            commands::power_telemetry_get,
            commands::user_list_get,
            // ── Save data + screenshot listing ──────────────────────
            commands::saves_list,
            commands::screenshots_list,
            // ── Filesystem search index (payload-side) ──────────────
            commands::fs_index_start,
            commands::fs_index_status,
            commands::fs_search_index,
            commands::fs_index_cancel,
            // ── App lifecycle (suspend/resume/kill/list) + toast ────
            commands::app_suspend,
            commands::app_resume,
            commands::app_kill,
            commands::app_list_running,
            commands::toast_push,
            // ── Diagnostics: klog stream, net interfaces, peripheral
            //    control (BD/USB power), loaded module enumeration ──
            commands::klog_chunk,
            commands::net_interfaces_get,
            commands::peripheral_eject,
            commands::peripheral_bd_off,
            commands::peripheral_bd_on,
            commands::peripheral_usb_off,
            commands::peripheral_usb_on,
            commands::proc_modules_get,
            // Shell + CRC32 + app.db query + net speed test
            commands::shell_run_cmd,
            commands::crc32_file_get,
            commands::appdb_query_get,
            commands::net_speed_test_run,
            commands::pkg_direct_mount_run,
            commands::ufs_fsck_run,
            commands::lwfs_mount_run,
            commands::fs_write_bytes_run,
            commands::fs_read_preview,
            commands::fs_blake3_hash,
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
            // ── Title metadata fetch (cover art + display title) ────
            // Server-side fetch (Rust/reqwest) of the public PS5
            // title-info site, bypassing renderer CSP/CORS. Hostname
            // allowlist enforced in commands/title_meta.rs.
            commands::title_meta_fetch,
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
