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
// The engine runs as a spawned sidecar binary on desktop, but in-process
// (linked library) on mobile, where Tauri has no sidecar model. Same
// `start`/`stop`/`url` surface either way; see engine.rs / engine_mobile.rs.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod engine;
#[cfg(any(target_os = "android", target_os = "ios"))]
#[path = "engine_mobile.rs"]
mod engine;

/// Build and run the Tauri application. The desktop `main.rs` calls this
/// directly; on mobile the `tauri::mobile_entry_point` macro generates
/// the JNI/Obj-C entry symbol the Android/iOS harness invokes (without
/// it the built `.so` fails Tauri's "missing required runtime symbols"
/// validation).
/// Open the main window centred and fully on-screen, shrinking it first if it
/// would overflow a small / low-res display ("crappy computers"). The
/// tauri.conf centre/size hints are applied at window creation and are
/// unreliable in practice (notably on macOS and under `tauri dev`), leaving
/// the window off-screen / off-centre — re-centring at runtime is reliable.
///
/// Desktop-only: the monitor / `center` / `set_size` window APIs don't exist
/// on mobile (where the app is full-screen anyway), so the mobile build gets a
/// no-op stub. (A previous version called these unconditionally and broke the
/// Android cross-compile: `no method named 'center' found`.)
#[cfg(desktop)]
fn center_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("main") {
        let monitor = win
            .current_monitor()
            .ok()
            .flatten()
            .or_else(|| win.primary_monitor().ok().flatten());
        if let (Some(monitor), Ok(size)) = (monitor, win.outer_size()) {
            let mon = monitor.size();
            let max_w = (mon.width as f64 * 0.95) as u32;
            let max_h = (mon.height as f64 * 0.90) as u32;
            if size.width > max_w || size.height > max_h {
                let _ = win.set_size(tauri::PhysicalSize::new(
                    size.width.min(max_w),
                    size.height.min(max_h),
                ));
            }
        }
        let _ = win.center();
    }
}

#[cfg(mobile)]
fn center_main_window<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Linux WebKitGTK white-screen rescue. On some GPU/compositor stacks
    // (Bazzite, SteamOS, NVIDIA, parts of Mesa) WebKitGTK's accelerated
    // compositing + DMABUF renderer produce a blank/white window. The
    // shipped `PS5Upload.sh` launcher exports these vars, but a user who
    // double-clicks the bare AppImage (or runs the .deb / folder build)
    // never goes through that wrapper. Setting them here — at the very
    // top of process startup, before the webview's child web process is
    // spawned and inherits our environment — applies the fix to every
    // launch method, not just the wrapper.
    //
    // Single-threaded at this point (no Tauri runtime, no engine thread
    // yet), so set_var is sound; edition 2021 keeps it safe (non-unsafe).
    // We only set when unset, so a user can still force the accelerated
    // path back with e.g. `WEBKIT_DISABLE_COMPOSITING_MODE=0 ...`.
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    let builder = tauri::Builder::default();
    // Single-instance MUST run before any other setup (window creation,
    // engine spawn). When a second launch hits, the runtime calls our
    // callback in the ORIGINAL process with the second instance's argv,
    // then exits the duplicate. This prevents the double-orphan-reap
    // race where launch #2's `engine::start()` would otherwise kill
    // launch #1's engine and leave its UI talking to a dead port.
    //
    // Desktop-only: a mobile app already has a single instance by
    // construction, and the plugin's `init` API isn't available on
    // Android/iOS. Rebinding `builder` behind a cfg keeps the fluent
    // chain below untouched.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
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
    }));
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        // Opener: the cross-platform way to open external URLs / reveal files.
        // Unlike the shell plugin's `open` (which tries to spawn a system
        // opener process and fails on Android with "No such file or directory"),
        // the opener plugin uses an Android Intent — so in-app links AND the
        // self-update "open the APK/release in the browser" flow work on mobile.
        .plugin(tauri_plugin_opener::init())
        // Native OS notifications. The renderer mirrors important in-app
        // inbox entries to the system notification center / Android shade
        // (see lib/osNotify.ts). Works on every platform.
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // Open the main window centred + fully on-screen. Desktop-only —
            // the centre/monitor/size window APIs don't exist on mobile, so
            // this is a no-op there (see center_main_window).
            center_main_window(app.handle());

            // Spawn the Rust engine binary as a sidecar. On failure we log and
            // keep the window open so the user can see diagnostic info — the
            // UI's passive status polling will flag the unreachable engine
            // without requiring a re-launch.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match engine::start(&handle).await {
                    Ok(url) => eprintln!("[tauri] engine ready at {url}"),
                    Err(e) => {
                        let message = format!("engine failed to start: {e}");
                        {
                            use tauri::Emitter;
                            let _ = handle.emit("ps5upload-engine-startup-error", &message);
                        }
                        eprintln!("[tauri] {message}");
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ── Engine proxies ──────────────────────────────────────
            // Thin wrappers over the ps5upload-engine HTTP API
            // (localhost:19113). Used by the Upload/Library/Volumes/
            // FileSystem/Hardware surfaces.
            commands::engine_url_set,
            commands::ps5_volumes,
            commands::pkg_scan_external,
            commands::pkg_metadata_console,
            commands::ps5_list_dir,
            commands::ps5_apps_installed,
            commands::ps5_readiness,
            commands::transfer_file,
            commands::transfer_dir,
            commands::transfer_zip,
            commands::zip_inspect,
            commands::zip_inspect_stream,
            commands::transfer_7z,
            commands::sevenz_inspect,
            commands::sevenz_inspect_stream,
            commands::transfer_rar,
            commands::rar_inspect,
            commands::profile_info,
            commands::profile_set_username,
            commands::profile_rename_user,
            commands::profile_activate,
            commands::profile_clear_slot,
            commands::profile_avatar_preview,
            commands::profile_avatar_current,
            commands::profile_apply_avatar,
            commands::transfer_dir_reconcile,
            commands::transfer_dir_diff_preview,
            commands::transfer_download,
            commands::transfer_download_zip,
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
            commands::ps5_syslog_tail,
            commands::ps5_hw_storage,
            commands::ps5_time_get,
            commands::ps5_time_sync,
            commands::ps5_time_state_get,
            commands::ps5_time_state_set,
            commands::ps5_hw_set_fan_threshold,
            commands::ps5_smp_meta_control,
            commands::ps5_smp_meta_stats,
            commands::ps5_app_launch,
            commands::ps5_app_register,
            commands::ps5_app_unregister,
            commands::job_status,
            commands::job_cancel,
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
            commands::pkg_dpi_install,
            commands::dpi_ensure,
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
            commands::payloads_releases,
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
            commands::process_list_get,
            commands::process_kill_pid,
            // ── Save data + screenshot listing ──────────────────────
            commands::saves_list,
            commands::screenshots_list,
            commands::videos_list,
            commands::screenshot_convert,
            // ── Save data .zip backup/restore ───────────────────────
            // The Saves screen uses these to wrap a downloaded save
            // folder into <title_id>.zip (and reverse on restore). The
            // download/upload bytes themselves still go through the
            // engine's transfer jobs; these commands only manage the
            // temp scratch dir and the zip envelope around it.
            commands::save_archive_make_temp,
            commands::save_archive_cleanup_temp,
            commands::save_archive_zip,
            commands::save_archive_unzip,
            // Format-aware backup/restore: strip `sdimg_` prefix from
            // PS4-style images and drop Sony's nested bookkeeping so
            // backups are clean and cross-tool compatible.
            commands::save_archive_backup_finalize,
            commands::save_archive_restore_prepare,
            // Generic text read/write at a user-picked path — backs the
            // bug-report bundle + settings export/import + search/stats
            // exports, which can't use plugin-fs read/writeTextFile
            // (dialog paths fall outside the fs scope). See
            // commands/save_text_file.rs.
            commands::save_text_file,
            commands::read_text_file,
            // ── Automatic crash/error report collection ─────────────
            // See commands/crash_reports.rs + src/lib/crashReporter.ts.
            commands::crash_reports::crash_report_save,
            commands::crash_reports::crash_reports_stats,
            commands::crash_reports::crash_reports_dir_resolved,
            commands::crash_reports::crash_reports_zip,
            commands::crash_reports::crash_reports_clear,
            commands::crash_reports::crash_reports_open_dir,
            // ── Persistent on-disk app log (bug-report time windows) ─
            // The renderer batches its unified log to ~/.ps5upload/logs/
            // so "package the last N minutes" survives a crash. See
            // commands/diag_log.rs + src/state/logs.ts.
            commands::diag_log::diag_log_append,
            commands::diag_log::diag_log_read_window,
            commands::diag_log::diag_log_stats,
            commands::diag_log::diag_log_clear,
            commands::diag_log::diag_log_open_dir,
            // ── Bug-report bundle (zip: logs + ps5 snapshot + images) ─
            // Assembles the one-click bug report. See commands/bug_report.rs
            // + src/screens/BugReport.
            commands::bug_report::bug_report_build,
            // ── In-app screenshot capture (bug-report gallery) ──────
            commands::bug_screenshots::screenshot_save,
            commands::bug_screenshots::screenshot_list,
            commands::bug_screenshots::screenshot_delete,
            commands::bug_screenshots::screenshot_clear,
            commands::bug_screenshots::screenshot_open_dir,
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
            commands::proc_list_get,
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
            // ── Persistence (Payloads → Send recent history) ────────
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
            commands::keep_awake_set, // spawn/kill platform sleep inhibitor (manual toggle)
            commands::keep_awake_state,
            commands::keep_awake_acquire, // refcounted hold by reason (auto: active transfer)
            commands::keep_awake_release,
            commands::user_config_load, // ~/.ps5upload/settings.json read
            commands::user_config_save, // ~/.ps5upload/settings.json atomic write
            commands::user_config_path_resolved, // show-the-path for the Settings UI
            commands::app_data_reset,   // factory-reset: wipe all local data/metadata
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
            // ── Local filesystem browse + Android all-files access ──
            // Backs the in-app file/folder picker used on Android (where
            // native dialogs return content:// URIs the engine can't
            // read). Harmless helpers on desktop.
            commands::local_list_dir,
            commands::local_storage_roots,
            commands::storage_access_granted,
            commands::request_storage_access,
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
                // Release the keep-awake inhibitor first. Its handle lives
                // in a `static` that isn't dropped at process exit, so an
                // enabled caffeinate/systemd-inhibit child would otherwise
                // outlive the app and block the machine from idle-sleeping
                // until reboot.
                commands::keep_awake_release_on_exit().await;
                engine::stop().await;
            });
        }
    });
}
