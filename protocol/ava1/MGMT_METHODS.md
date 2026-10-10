# Management methods (AVA1 :9120)

Every management method the payload serves over AVA1: its number and name, its body, the payload
handler and the engine/core caller. The payload's table is `payload/src/mgmt_table.def` (dispatched by
`payload/src/mgmt_rpc.c`); the engine's is `ps5upload-core/src/mgmt.rs` (`m::*`). Method numbers and
bodies are normative in `schema/ava1.toml` and SPEC.md §7.3. Long-running work runs as `job.run` ops
(the `(op …)` rows).

`frame` is the handler's internal frame number (the `MGMT_FRAME_*` pair `mgmt_table.def` names), kept
from the protocol AVA1 replaced; `—` marks a method that never had one. `host tests` names the
`ava1-ctest` (payload C) and `ps5upload-ava1` (engine) tests that cover a method beyond the table-wide
ones (`ava1-ctest/tests/mgmt*.rs`, `ps5upload-ava1/tests/mgmt*.rs`). Methods 119 and 120 (FTP) and
123 (`notif.send`) are retired and must not be reused.

| frame | method | body | payload handler | engine/core caller | host tests |
|---|---|---|---|---|---|
| Hello 1 | 1 `node.info` | empty -> `NodeInfo` | `src/ava1_glue.c` (answered before the table) | none | `ava1-ctest/tests/server.rs` `rust_client_talks_to_the_c_server` (`node_info`, oversize body) |
| Status 20 | 4 `node.status` | empty -> `NodeStatus` | `src/runtime.c` `handle_status_frame` | `ps5upload-core/src/health.rs`, `ps5upload-engine/src/lib.rs` | `ava1-ctest/tests/mgmt.rs` `c_mgmt_node_status_is_typed`; engine `ps5upload-ava1/tests/mgmt.rs` `a_typed_node_status_rebuilds_the_legacy_json_with_a_bool_and_replaced` |
| Shutdown 22 | 5 `node.shutdown` | empty -> empty (the reply is written first, the exit starts 300 ms later on a deferred thread, then `ava1_payload_stop`) | `src/runtime.c` `handle_node_shutdown` | `ps5upload-core/src/payload_lifecycle.rs` |  |
| Cleanup 32 | 6 `node.cleanup` | `MgmtText`; a long tree runs as `job.run (op CLEANUP)` (payload op, Task 5) | the engine runs it as `job.run (op CLEANUP)`: `src/runtime.c` `handle_cleanup` | `ps5upload-core/src/cleanup.rs` | host-tested |
| FsListVolumes 34 | 32 `fs.volumes` | `MgmtText` | `src/runtime.c` `handle_fs_list_volumes` | `ps5upload-core/src/volumes.rs` | host-tested |
| FsListDir 36 | 33 `fs.list` | `FsList` -> `FsListResult` (<= 256 entries per call, `more`) | native `src/mgmt_fs.c` `mgmt_run_fs_list` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsHash 38 | 20 `job.run (op HASH)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_hash` | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/job_run.rs` `hash_result_rides_in_status_ext`; engine `ps5upload-ava1/tests/mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
| FsDelete 40 | 20 `job.run (op DELETE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_delete` | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `cancel_stops_a_delete_midway_and_the_job_is_collected_after`; engine `mgmt_job.rs` `fs_delete_over_ava1_is_a_job_and_a_cancel_is_the_word_cancelled` |
| FsMove 42 | 36 `fs.rename` | `FsRename` -> empty; `ERR_CROSS_DEVICE` | native `src/mgmt_fs.c` `mgmt_run_fs_rename` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsChmod 44 | 37 `fs.chmod` | `FsChmod` -> empty (recursive: `job.run` CHMOD_R) | native `src/mgmt_fs.c` `mgmt_run_fs_chmod` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsMkdir 46 | 35 `fs.mkdir` | `FsMkdir` -> empty | native `src/mgmt_fs.c` `mgmt_run_fs_mkdir` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsRead 48 | 38 `fs.read` | `FsRead` -> `FsReadResult` (len <= FS_READ_MAX, loop on `eof`) | native `src/mgmt_fs.c` `mgmt_run_fs_read` | `ps5upload-core/src/download.rs`, `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsCopy 50 | 16 `job.copy` | `JobCopy` | `ava1/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/copy.rs` `a_folder_copies_on_the_console`, `a_move_deletes_the_source_only_after_a_verified_copy`; engine `ps5upload-ava1/tests/download_copy.rs` `a_console_copy_polls_status_until_done` |
| FsMount 52 | 40 `fs.mount` | `MgmtText` | `src/runtime.c` `handle_fs_mount` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsUnmount 54 | 41 `fs.unmount` | `MgmtText` | `src/runtime.c` `handle_fs_unmount` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| AppRegister 56 | 48 `app.register` | `MgmtText` | `src/runtime.c` `handle_app_register` | `ps5upload-core/src/fs_ops.rs` |  |
| AppUnregister 58 | 49 `app.unregister` | `MgmtText` | `src/runtime.c` `handle_app_unregister` | `ps5upload-core/src/fs_ops.rs` |  |
| AppLaunch 60 | 50 `app.launch` | `MgmtText` | `src/runtime.c` `handle_app_launch` | `ps5upload-core/src/fs_ops.rs` |  |
| AppListRegistered 62 | 51 `app.list` | `MgmtText` (pages: offset/limit, `more`) | `src/runtime.c` `handle_app_list_registered` | `ps5upload-core/src/fs_ops.rs` |  |
| HwInfo 64 | 72 `hw.info` | `MgmtText` | `src/runtime.c` `handle_hw_info` | `ps5upload-core/src/hw.rs` |  |
| HwTemps 66 | 73 `hw.temps` | `MgmtText` | `src/runtime.c` `handle_hw_temps` | `ps5upload-core/src/hw.rs` |  |
| HwPower 68 | 74 `hw.power` | `MgmtText` | `src/runtime.c` `handle_hw_power` | `ps5upload-core/src/hw.rs` |  |
| AppLaunchBrowser 70 | 52 `app.launch_browser` | `MgmtText` | `src/runtime.c` `handle_app_launch_browser` | `ps5upload-core/src/hw.rs` |  |
| HwSetFanThreshold 72 | 76 `hw.fan_threshold` | `MgmtText` | `src/runtime.c` `handle_hw_set_fan_threshold` | `ps5upload-core/src/hw.rs` |  |
| ProcList 74 | 58 `proc.list` | `MgmtText` | `src/runtime.c` `handle_proc_list` | `ps5upload-core/src/hw.rs` |  |
| FsOpStatus 76 | 17 `job.status` | `JobRef` -> `Status` | `ava1/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `status_survives_a_reconnect`; engine `download_copy.rs` `a_console_copy_polls_status_until_done` |
| FsOpCancel 78 | 18 `job.cancel` | `JobRef` -> empty | `ava1/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/job_run.rs` `cancel_stops_a_delete_midway_and_the_job_is_collected_after`, `another_device_cannot_see_cancel_or_reuse_an_operation`; engine `download_copy.rs` `a_console_copy_cancel_signals_the_job` |
| HwStorage 80 | 75 `hw.storage` | `MgmtText` | `src/runtime.c` `handle_hw_storage` | `ps5upload-core/src/hw.rs` |  |
| SystemControl 86 | 80 `power.control` | `MgmtText` | `src/runtime.c` `handle_system_control` | `ps5upload-core/src/system_control.rs` |  |
| PowerTelemetry 88 | 81 `power.telemetry` | `MgmtText` | `src/runtime.c` `handle_power_telemetry` | `ps5upload-core/src/system_control.rs` |  |
| UserList 90 | 94 `user.list` | `MgmtText` | `src/runtime.c` `handle_user_list` | none |  |
| ListSaves 92 | 64 `saves.list` | `MgmtText` | `src/runtime.c` `handle_list_saves` | `ps5upload-core/src/saves.rs` |  |
| ListScreenshots 94 | 65 `shots.list` | `MgmtText` | `src/runtime.c` `handle_list_screenshots` | `ps5upload-core/src/saves.rs` |  |
| IndexStart 96 | 67 `index.start` | `MgmtText` | `src/runtime.c` `handle_index_start` | none |  |
| IndexStatus 98 | 68 `index.status` | `MgmtText` | `src/runtime.c` `handle_index_status` | none |  |
| SearchIndex 100 | 69 `index.search` | `MgmtText` | `src/runtime.c` `handle_search_index` | none |  |
| IndexCancel 102 | 70 `index.cancel` | `MgmtText` | `src/runtime.c` `handle_index_cancel` | none |  |
| AppLifecycle 104 | 53 `app.lifecycle` | `MgmtText` | `src/runtime.c` `handle_app_lifecycle` | `ps5upload-core/src/app_lifecycle.rs` |  |
| ToastSend 106 | 125 `toast.send` | `MgmtText` | `src/runtime.c` `mgmt_w_toast_send` | `ps5upload-core/src/app_lifecycle.rs` |  |
| KlogRead 108 | 7 `log.klog` | `MgmtText` {max_bytes} -> text | `src/runtime.c` `handle_klog_read` | `ps5upload-core/src/diagnostics.rs` |  |
| NetInterfaces 110 | 9 `net.interfaces` | `MgmtText` | `src/runtime.c` `handle_net_interfaces` | `ps5upload-core/src/diagnostics.rs` |  |
| PeripheralControl 112 | 86 `periph.control` | `MgmtText` | `src/runtime.c` `handle_peripheral_control` | `ps5upload-core/src/diagnostics.rs` |  |
| ProcModules 114 | 61 `proc.modules` | `MgmtText` | `src/runtime.c` `handle_proc_modules` | `ps5upload-core/src/diagnostics.rs` |  |
| ShellExec 116 | 87 `shell.exec` | `MgmtText` | `src/runtime.c` `handle_shell_exec` | `ps5upload-core/src/diagnostics.rs` |  |
| Crc32File 118 | 20 `job.run (op CRC32)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_crc32` | `ps5upload-core/src/diagnostics.rs` | `ava1-ctest/tests/job_run.rs` `crc32_of_a_large_file_is_a_job_that_can_be_cancelled`; engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
| AppDbQuery 120 | 56 `app.db_query` | `MgmtText` | `src/runtime.c` `handle_appdb_query` | `ps5upload-core/src/diagnostics.rs` |  |
| NetSpeedTest 122 | 11 `net.speedtest` | `MgmtText` | `src/runtime.c` `handle_net_speed_test` | `ps5upload-core/src/diagnostics.rs` |  |
| PkgDirectMount 124 | 42 `fs.mount_pkg` | `MgmtText` | `src/runtime.c` `handle_pkg_direct_mount` | `ps5upload-core/src/diagnostics.rs` | host-tested |
| UfsFsck 126 | 20 `job.run (op FSCK)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_ufs_fsck` | `ps5upload-core/src/diagnostics.rs` | `ava1-ctest/tests/job_run.rs` `fsck_keeps_its_ok_false_body_as_the_answer_and_a_frame_error_is_a_failure`; engine `mgmt_job.rs` (same shapes test) |
| LwfsMount 128 | 43 `fs.mount_lwfs` | `MgmtText` | `src/runtime.c` `handle_lwfs_mount` | `ps5upload-core/src/diagnostics.rs` | host-tested |
| FsWriteBytes 130 | 39 `fs.write` | `FsWrite` -> empty (chunks <= FSW_CHUNK_MAX, SPEC §7.5) | native `src/mgmt_fs.c` `mgmt_run_fs_write` | `ps5upload-core/src/diagnostics.rs` | host-tested |
| TimeGet 132 | 82 `time.get` | `MgmtText` | `src/runtime.c` `handle_time_get` | `ps5upload-core/src/sys_time.rs` |  |
| TimeSet 134 | 83 `time.set` | `MgmtText` | `src/runtime.c` `handle_time_set` | `ps5upload-core/src/sys_time.rs` |  |
| TimeStateGet 136 | 84 `time.state_get` | `MgmtText` | `src/runtime.c` `handle_time_state_get` | none |  |
| TimeStateSet 138 | 85 `time.state_set` | `MgmtText` | `src/runtime.c` `handle_time_state_set` | none |  |
| SmpMetaControl 140 | 112 `smp.meta_control` | `MgmtText` | `src/runtime.c` `handle_smp_meta_control` | `ps5upload-core/src/smp_meta.rs` |  |
| SmpMetaStats 142 | 113 `smp.meta_stats` | `MgmtText` | `src/runtime.c` `handle_smp_meta_stats` | `ps5upload-core/src/smp_meta.rs` |  |
| SyslogTail 144 | 8 `log.syslog` | `MgmtText` {max_bytes} -> text | `src/runtime.c` `handle_syslog_tail` | `ps5upload-core/src/hw.rs` |  |
| NetReach 148 | 10 `net.reach` | `MgmtText` | `src/runtime.c` `handle_net_reach` | `ps5upload-core/src/diagnostics.rs` |  |
| ProfileInfo 150 | 88 `profile.info` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_info` | `ps5upload-core/src/profile.rs` |  |
| ProfileSetUsername 152 | 89 `profile.set_username` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_set_username` | `ps5upload-core/src/profile.rs` |  |
| ProfileActivate 154 | 90 `profile.activate` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_activate` | `ps5upload-core/src/profile.rs` |  |
| ProfileApplyAvatar 156 | 91 `profile.apply_avatar` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_apply_avatar` | `ps5upload-core/src/profile.rs` |  |
| ProfileClearSlot 158 | 92 `profile.clear_slot` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_clear_slot` | `ps5upload-core/src/profile.rs` |  |
| ProfileSetLocalUsername 160 | 93 `profile.set_local_username` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_set_local_username` | `ps5upload-core/src/profile.rs` |  |
| ProcessList 162 | 59 `proc.process_list` | `MgmtText` | `src/runtime.c` `handle_process_list` | `ps5upload-core/src/process_mgr.rs` |  |
| ProcessKill 164 | 60 `proc.kill` | `MgmtText` | `src/runtime.c` `handle_process_kill` | `ps5upload-core/src/process_mgr.rs` |  |
| ListVideos 166 | 66 `videos.list` | `MgmtText` | `src/runtime.c` `handle_list_videos` | `ps5upload-core/src/saves.rs` |  |
| HwDriveSensors 168 | 79 `hw.drive_sensors` | `MgmtText` | `src/runtime.c` `handle_hw_drive_sensors` | `ps5upload-core/src/hw.rs` |  |
| UserCreate 170 | 95 `user.create` | `MgmtText` | `src/runtime.c` `mgmt_w_user_create` | `ps5upload-core/src/users.rs` |  |
| UserDelete 172 | 96 `user.delete` | `MgmtText` | `src/runtime.c` `mgmt_w_user_delete` | `ps5upload-core/src/users.rs` |  |
| FocusProbe 174 | 57 `proc.focus` | `MgmtText` | `src/runtime.c` `handle_focus_probe` | `ps5upload-core/src/focus.rs` |  |
| BackupSnapshot 176 | 20 `job.run (op BACKUP_SNAPSHOT)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_backup_snapshot` | `ps5upload-core/src/backup.rs` | `ava1-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it`; engine `mgmt_job.rs` `a_job_the_console_forgot_is_started_again_except_a_snapshot` |
| BackupList 178 | 98 `backup.list` | `MgmtText` | `src/runtime.c` `mgmt_w_backup_list` | `ps5upload-core/src/backup.rs` |  |
| BackupRestore 180 | 20 `job.run (op BACKUP_RESTORE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_backup_restore` | `ps5upload-core/src/backup.rs` | `ava1-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it` (also runs the restore op); engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
| BackupDelete 182 | 100 `backup.delete` | `MgmtText` | `src/runtime.c` `mgmt_w_backup_delete` | `ps5upload-core/src/backup.rs` |  |
| RemotePlayRequest 188 | 136 `rp.request` | `MgmtText` | `src/runtime.c` `mgmt_w_remoteplay_request` | `ps5upload-core/src/remoteplay.rs` |  |
| RemotePlayStatus 189 | 137 `rp.status` | `MgmtText` | `src/runtime.c` `handle_remoteplay_status` | `ps5upload-core/src/remoteplay.rs` |  |
| RemotePlayCancel 190 | 138 `rp.cancel` | `MgmtText` | `src/runtime.c` `handle_remoteplay_cancel` | `ps5upload-core/src/remoteplay.rs` |  |
| ActivityGet 192 | 126 `activity.get` | `MgmtText` | `src/runtime.c` `handle_activity_get` | `ps5upload-core/src/activity.rs` |  |
| ActivityDbQuery 194 | 127 `activity.db_query` | `MgmtText` | `src/runtime.c` `mgmt_w_activity_db_query` | `ps5upload-core/src/activity.rs` |  |
| HwFanCurveSet 196 | 77 `hw.fan_curve_set` | `MgmtText` | `src/runtime.c` `mgmt_w_fan_curve_set` | none |  |
| NotifList 198 | 122 `notif.list` | `MgmtText` | `src/runtime.c` `mgmt_w_notif_list` | `ps5upload-core/src/notif.rs` |  |
| CheatsList 200 | 104 `cheats.list` | `MgmtText` | `src/runtime.c` `handle_cheats_list` | `ps5upload-core/src/cheats.rs` |  |
| CheatsGet 202 | 105 `cheats.get` | `MgmtText` | `src/runtime.c` `mgmt_w_cheats_get` | `ps5upload-core/src/cheats.rs` |  |
| CheatsToggle 204 | 106 `cheats.toggle` | `MgmtText` | `src/runtime.c` `mgmt_w_cheats_toggle` | `ps5upload-core/src/cheats.rs` |  |
| CheatsDelete 206 | 107 `cheats.delete` | `MgmtText` | `src/runtime.c` `mgmt_w_cheats_delete` | `ps5upload-core/src/cheats.rs` |  |
| CheatsReload 208 | 108 `cheats.reload` | `MgmtText` | `src/runtime.c` `handle_cheats_reload` | `ps5upload-core/src/cheats.rs` |  |
| CheatsStatus 210 | 109 `cheats.status` | `MgmtText` | `src/runtime.c` `handle_cheats_status` | `ps5upload-core/src/cheats.rs` |  |
| CheatsEngineSet 212 | 110 `cheats.engine_set` | `MgmtText` | `src/runtime.c` `mgmt_w_cheats_engine_set` | `ps5upload-core/src/cheats.rs` |  |
| SdkScan 214 | 114 `sdk.scan` | `MgmtText`; also `job.run (op SDK_SCAN)` (payload op, Task 5) | `src/runtime.c` `handle_sdk_scan` | `ps5upload-core/src/sdk_changer.rs` |  |
| SdkPatch 216 | 115 `sdk.patch` | `MgmtText` | `src/runtime.c` `mgmt_w_sdk_patch` | `ps5upload-core/src/sdk_changer.rs` |  |
| SdkRestore 218 | 116 `sdk.restore` | `MgmtText` | `src/runtime.c` `mgmt_w_sdk_restore` | `ps5upload-core/src/sdk_changer.rs` |  |
| TmdbFetch 222 | 117 `tmdb.fetch` | `MgmtText` | `src/runtime.c` `mgmt_w_tmdb_fetch` | none |  |
| TmdbStore 228 | 118 `tmdb.store` | `MgmtText` | `src/runtime.c` `mgmt_w_tmdb_store` | none |  |
| FwSpoofStatus 232 | 121 `fwspoof.status` | `MgmtText` | `src/runtime.c` `handle_fw_spoof_status` | `ps5upload-core/src/fw_spoof.rs` |  |
| AppInfoQuery 234 | 54 `app.info_query` | `MgmtText` | `src/runtime.c` `handle_appinfo_query` | `ps5upload-core/src/diagnostics.rs` |  |
| AppInfoSet 236 | 55 `app.info_set` | `MgmtText` | `src/runtime.c` `handle_appinfo_set` | none |  |
| HwFanCurveGet 246 | 78 `hw.fan_curve_get` | `MgmtText` | `src/runtime.c` `handle_fan_curve_get` | none |  |
| RemotePlayReadiness 248 | 139 `rp.readiness` | `MgmtText` | `src/runtime.c` `handle_remoteplay_readiness` | `ps5upload-core/src/remoteplay.rs` |  |
| RemotePlayEnable 249 | 140 `rp.enable` | `MgmtText` | `src/runtime.c` `mgmt_w_remoteplay_enable` | `ps5upload-core/src/remoteplay.rs` |  |
| RemotePlayDevices 250 | 141 `rp.devices` | `MgmtText` | `src/runtime.c` `handle_remoteplay_devices` | none |  |
| NotifClear 251 | 124 `notif.clear` | `MgmtText` | `src/runtime.c` `handle_notif_clear` | `ps5upload-core/src/notif.rs` |  |
| ActivityReset 253 | 128 `activity.reset` | `MgmtText` | `src/runtime.c` `handle_activity_reset` | `ps5upload-core/src/activity.rs` |  |
| — | 21 `job.list` | empty -> `JobListResult` | `ava1/ava1_data.c` (the data plane) | none | `ava1-ctest/tests/job_run.rs` `repeat_job_run_is_idempotent`, `at_most_eight_operations_run_at_once_and_an_unknown_op_is_refused` (listing); no engine caller yet |
| — | 34 `fs.stat` | `FsPath` -> `FsStat` | native `src/mgmt_fs.c` `mgmt_run_fs_stat` | none | host-tested |
| — | 44 `fs.freespace` | `FsPath` -> `FsFreeSpace{usable, free, total, reserve, dev}`: the room an upload may fill on the drive holding the path (nearest existing ancestor): free less the working margin (1/64th, at most 1 GiB), never the memory admit budget or the hidden allocator pool; the engine's up-front space check (review 015 #02) | native `src/mgmt_fs.c` `mgmt_run_fs_freespace` | `ps5upload-core/src/volumes.rs` `free_space` (`ps5upload-ava1/src/space.rs`) | `ava1-ctest/tests/mgmt_fs.rs` `fs_freespace_is_the_post_reserve_figure_of_the_drive_holding_the_path`; engine `ps5upload-ava1/tests/mgmt.rs` |

## Reply sizes (SPEC.md §7.4)

The RPC reply cap is 256 KiB (`RPC_REPLY_MAX`); a `MgmtText` reply holds at most `RPC_TEXT_MAX` = 262,128 bytes of text
(6 bytes of encoding, 13 with `more`). "Max reply" is the largest reply buffer the handler uses (so the largest reply it
can send). Decisions: **fits** (no change), **pages** (offset/limit and `more`), or a clamp. Exactly
one handler (`app.list`) needs paging; `fs.read` pages by its caller; everything else fits. Ported handlers still
detect truncation (SPEC §7.3): a buffer smaller than the answer is `ERR_INTERNAL`, never a clipped `ok`.

| method | max reply | decision |
|---|---|---|
| node.status | ~0.7 KiB (`NodeStatus`) | fits |
| node.cleanup | <= 0.3 KiB | fits |
| log.klog | <= 64 KiB (clamped `max_bytes`) | fits; if it ever did not, the tail rule below applies |
| log.syslog | up to 1 MiB (`HARD_CAP`) | **clamped tail** (`mgmt_call_tail`): the reply is the newest `RPC_TEXT_MAX` bytes, cut at a line start (else a UTF-8 boundary), `more` = 1 when older text was left out; the Rust transport leads such a reply with a one-line note |
| net.interfaces | < 4 KiB (16 interfaces) | fits |
| net.reach, net.speedtest | <= 0.3 KiB | fits |
| fs.volumes | 16 KiB (`RESP_CAP`) | fits |
| fs.list | 32 KiB, <= 256 entries | fits (`FsListResult`, `more` past 256 entries) |
| fs.mount, fs.unmount, fs.mount_pkg, fs.mount_lwfs | <= 2 KiB | fits |
| fs.read | up to 2 MiB per call (`FS_READ_MAX_BYTES`) | pages by the caller: `len <= FS_READ_MAX` (262,128), loop on `eof` |
| fs.write, fs.mkdir, fs.rename, fs.chmod | <= 0.2 KiB | fits |
| app.register, app.unregister, app.launch, app.launch_browser, app.lifecycle | <= 1 KiB | fits |
| app.list | 512 KiB buffer, ~3,800 entries | **pages with offset/more**: the one handler that does not fit 256 KiB (typical consoles: tens of KiB) |
| app.info_query | 32 KiB | fits |
| app.info_set | <= 1 KiB | fits |
| app.db_query | 64 KiB | fits |
| proc.focus | 8 KiB | fits |
| proc.list | 64 KiB | fits |
| proc.process_list | 256 KiB buffer | fits when the buffer is sized `RPC_TEXT_MAX` (the JSON must be 16 bytes shorter than the legacy buffer) |
| proc.kill | <= 0.2 KiB | fits |
| proc.modules | 64 KiB | fits |
| saves.list, shots.list, videos.list | 512 KiB each (`LIST_JSON_CAP`, was 64 KiB) | **pages with offset/more** (the engine merges the pages); a walk that fills the buffer closes with `"truncated":true` instead of looking complete |
| index.start, index.status, index.cancel | <= 1 KiB | fits |
| index.search | 256 KiB buffer, up to 5000 hits | fits (the handler stops 2.3 KiB short of its buffer, under `RPC_TEXT_MAX`); hits past it are dropped as before and the reply now says `"truncated":true`. Not paged: the request's own `limit` is the hit count |
| hw.info, hw.temps, hw.power, hw.storage, hw.drive_sensors | 2 KiB (`handle_hw_text_op`) | fits |
| hw.fan_threshold, hw.fan_curve_set | <= 0.4 KiB | fits |
| hw.fan_curve_get | 4 KiB | fits |
| power.control, power.telemetry | <= 0.5 KiB | fits |
| time.get, time.set, time.state_set | <= 1 KiB | fits |
| time.state_get | 2 KiB | fits |
| periph.control | <= 0.2 KiB | fits |
| shell.exec | shell output (`shell_builtin.c`), bounded by the 10 s limit | clamp to `RPC_TEXT_MAX`; Task 7 audits the builtin |
| profile.info | 4 KiB | fits |
| profile.* (others), user.create, user.delete | <= 0.4 KiB | fits |
| user.list | 4 KiB | fits |
| backup.list | 32 KiB | fits |
| backup.delete | <= 0.2 KiB | fits |
| cheats.list, cheats.get | 64 KiB (`CHEATS_JSON_BUF_SZ`) | fits |
| cheats.toggle, .delete, .reload, .status, .engine_set | <= 0.5 KiB | fits |
| smp.meta_control, smp.meta_stats | <= 0.4 KiB | fits |
| sdk.scan | 64 KiB (`SDK_JSON_BUF_SZ`) | fits |
| sdk.patch, sdk.restore | <= 1 KiB | fits |
| tmdb.fetch, tmdb.store | 64 KiB (`TMDB_JSON_BUF_SZ`) | fits |
| fwspoof.status | <= 1 KiB | fits |
| notif.list | 32 KiB | fits |
| notif.clear, toast.send | <= 0.1 KiB | fits |
| activity.get, activity.db_query | 128 KiB (`ACTIVITY_JSON_BUF_SZ`) | fits |
| activity.reset | <= 0.1 KiB | fits |
| rp.request, rp.status, rp.cancel, rp.readiness, rp.enable, rp.devices | <= 2 KiB | fits |
