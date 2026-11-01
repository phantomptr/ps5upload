# Management methods (AVA1 :9120)

Every management method the payload serves over AVA1: its number and name, its body, the payload
handler and the engine/core caller. The payload's table is `payload/src/mgmt_table.def` (dispatched by
`payload/src/mgmt_rpc.c`); the engine's is `ps5upload-core/src/mgmt.rs` (`m::*`). Method numbers,
bodies and `JOB_OP_*` numbers are normative in `app.toml` (this directory); the protocol's own
methods (1–3, 16–23) and the rules every method follows are in `third_party/ava1/spec/SPEC.md` §7, and
ps5upload's per-method rules are in `PROFILE.md`. Long-running work runs as `job.run` ops (the
`(op …)` rows).

`frame` is the handler's internal frame number (the `MGMT_FRAME_*` pair `mgmt_table.def` names), kept
from FTX2, the protocol AVA1 replaced (PROFILE.md §9); `—` marks a method that never had one. `host tests`
names the `ps5upload-ctest` (payload C, `engine/crates/ps5upload-ctest`), `ava1-ctest` (the library's C,
`third_party/ava1/crates/ava1-ctest`) and `ps5upload-ava1` (engine) tests that cover a method beyond the
table-wide ones (`ps5upload-ctest/tests/mgmt*.rs`, `ps5upload-ava1/tests/mgmt*.rs`). Methods 119 and 120 (FTP) and
123 (`notif.send`) are retired and must not be reused.

| frame | method | body | payload handler | engine/core caller | host tests |
|---|---|---|---|---|---|
| Hello 1 | 1 `node.info` | empty -> `NodeInfo` | `src/ava1_glue.c` (answered before the table) | none | `ava1-ctest/tests/server.rs` `rust_client_talks_to_the_c_server` (`node_info`, oversize body) |
| Status 20 | 4 `node.status` | empty -> `NodeStatus` | `src/runtime.c` `handle_status_frame` | `ps5upload-core/src/health.rs`, `ps5upload-engine/src/lib.rs` | `ps5upload-ctest/tests/mgmt.rs` `c_mgmt_node_status_is_typed`; engine `ps5upload-ava1/tests/mgmt.rs` `a_typed_node_status_rebuilds_the_legacy_json_with_a_bool_and_replaced` |
| Shutdown 22 | 5 `node.shutdown` | empty -> empty (the reply is written first, the exit starts 300 ms later on a deferred thread, then `ava1_payload_stop`) | `src/runtime.c` `handle_node_shutdown` | `ps5upload-core/src/payload_lifecycle.rs` |  |
| Cleanup 32 | 6 `node.cleanup` | `MgmtText`; a long tree runs as `job.run (op CLEANUP)` (payload op, Task 5) | the engine runs it as `job.run (op CLEANUP)`: `src/runtime.c` `handle_cleanup` | `ps5upload-core/src/cleanup.rs` | host-tested |
| FsListVolumes 34 | 32 `fs.volumes` | `MgmtText` | `src/runtime.c` `handle_fs_list_volumes` | `ps5upload-core/src/volumes.rs` | host-tested |
| FsListDir 36 | 33 `fs.list` | `FsList` -> `FsListResult` (<= 256 entries per call, `more`) | native `src/mgmt_fs.c` `mgmt_run_fs_list` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsHash 38 | 20 `job.run (op HASH)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_hash` | `ps5upload-core/src/fs_ops.rs` | `ps5upload-ctest/tests/job_run.rs` `hash_result_rides_in_status_ext`; engine `ps5upload-ava1/tests/mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
| FsDelete 40 | 20 `job.run (op DELETE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_delete` | `ps5upload-core/src/fs_ops.rs` | `ps5upload-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `cancel_stops_a_delete_midway_and_the_job_is_collected_after`; engine `mgmt_job.rs` `fs_delete_over_ava1_is_a_job_and_a_cancel_is_the_word_cancelled` |
| FsMove 42 | 36 `fs.rename` | `FsRename` -> empty; `ERR_CROSS_DEVICE` | native `src/mgmt_fs.c` `mgmt_run_fs_rename` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsChmod 44 | 37 `fs.chmod` | `FsChmod` -> empty (recursive: `job.run` CHMOD_R) | native `src/mgmt_fs.c` `mgmt_run_fs_chmod` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsMkdir 46 | 35 `fs.mkdir` | `FsMkdir` -> empty | native `src/mgmt_fs.c` `mgmt_run_fs_mkdir` | `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsRead 48 | 38 `fs.read` | `FsRead` -> `FsReadResult` (len <= FS_READ_MAX, loop on `eof`) | native `src/mgmt_fs.c` `mgmt_run_fs_read` | `ps5upload-core/src/download.rs`, `ps5upload-core/src/fs_ops.rs` | host-tested |
| FsCopy 50 | 16 `job.copy` | `JobCopy` | `third_party/ava1/c/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ava1-ctest/tests/copy.rs` `a_folder_copies_on_the_console`, `a_move_deletes_the_source_only_after_a_verified_copy`; engine `ps5upload-ava1/tests/download_copy.rs` `a_console_copy_polls_status_until_done` |
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
| FsOpStatus 76 | 17 `job.status` | `JobRef` -> `Status` | `third_party/ava1/c/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ps5upload-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `status_survives_a_reconnect`; `c2c.rs` `another_paired_device_reads_and_cancels_a_c2c_send` (a transfer answers any paired device with `CAP_SHARED_JOBS`, SPEC §18.5), `operation_jobs_stay_owner_only`; engine `download_copy.rs` `a_console_copy_polls_status_until_done` |
| FsOpCancel 78 | 18 `job.cancel` | `JobRef` -> empty | `third_party/ava1/c/ava1_data.c` (the data plane) | `ps5upload-core/src/fs_ops.rs` | `ps5upload-ctest/tests/job_run.rs` `cancel_stops_a_delete_midway_and_the_job_is_collected_after`, `another_device_cannot_see_cancel_or_reuse_an_operation`; `c2c.rs` `another_paired_device_reads_and_cancels_a_c2c_send`, `operation_jobs_stay_owner_only`; engine `download_copy.rs` `a_console_copy_cancel_signals_the_job` |
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
| Crc32File 118 | 20 `job.run (op CRC32)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/fs_jobs.c` `op_crc32` | `ps5upload-core/src/diagnostics.rs` | `ps5upload-ctest/tests/job_run.rs` `crc32_of_a_large_file_is_a_job_that_can_be_cancelled`; engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
| AppDbQuery 120 | 56 `app.db_query` | `MgmtText`; request `{"offset":N,"limit":M}` (both optional, empty = page one), reply adds `offset`, `next`, `more`, `truncated` (#473) | `src/runtime.c` `handle_appdb_query` | `ps5upload-core/src/diagnostics.rs` (pages until `more` is false) |  |
| NetSpeedTest 122 | 11 `net.speedtest` | `MgmtText` | `src/runtime.c` `handle_net_speed_test` | `ps5upload-core/src/diagnostics.rs` |  |
| PkgDirectMount 124 | 42 `fs.mount_pkg` | `MgmtText` | `src/runtime.c` `handle_pkg_direct_mount` | `ps5upload-core/src/diagnostics.rs` | host-tested |
| UfsFsck 126 | 20 `job.run (op FSCK)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_ufs_fsck` | `ps5upload-core/src/diagnostics.rs` | `ps5upload-ctest/tests/job_run.rs` `fsck_keeps_its_ok_false_body_as_the_answer_and_a_frame_error_is_a_failure`; engine `mgmt_job.rs` (same shapes test) |
| LwfsMount 128 | 43 `fs.mount_lwfs` | `MgmtText` | `src/runtime.c` `handle_lwfs_mount` | `ps5upload-core/src/diagnostics.rs` | host-tested |
| FsWriteBytes 130 | 39 `fs.write` | `FsWrite` -> empty (chunks <= FSW_CHUNK_MAX, PROFILE.md §6) | native `src/mgmt_fs.c` `mgmt_run_fs_write` | `ps5upload-core/src/diagnostics.rs` | host-tested |
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
| ProfileNpState 334 | 182 `profile.np_state` | `MgmtText` {slot, uid} -> sign-in values + whether a backup is kept | `src/runtime.c` `mgmt_w_profile_np_state` (`src/profile.c`) | `ps5upload-core/src/profile.rs` |  |
| ProfileNpBackup 336 | 183 `profile.np_backup` | `MgmtText` {slot, uid} -> {ok, created}: keep what np-fake-signin overwrites | `src/runtime.c` `mgmt_w_profile_np_backup` (`src/profile.c`) | `ps5upload-core/src/profile.rs` |  |
| ProfileNpRestore 338 | 184 `profile.np_restore` | `MgmtText` {uid} -> {ok, written, failed}: put the kept copy back | `src/runtime.c` `mgmt_w_profile_np_restore` (`src/profile.c`) | `ps5upload-core/src/profile.rs` |  |
| ProfileSetLocalUsername 160 | 93 `profile.set_local_username` | `MgmtText` | `src/runtime.c` `mgmt_w_profile_set_local_username` | `ps5upload-core/src/profile.rs` |  |
| ProcessList 162 | 59 `proc.process_list` | `MgmtText` | `src/runtime.c` `handle_process_list` | `ps5upload-core/src/process_mgr.rs` |  |
| ProcessKill 164 | 60 `proc.kill` | `MgmtText` | `src/runtime.c` `handle_process_kill` | `ps5upload-core/src/process_mgr.rs` |  |
| ListVideos 166 | 66 `videos.list` | `MgmtText` | `src/runtime.c` `handle_list_videos` | `ps5upload-core/src/saves.rs` |  |
| HwDriveSensors 168 | 79 `hw.drive_sensors` | `MgmtText` | `src/runtime.c` `handle_hw_drive_sensors` | `ps5upload-core/src/hw.rs` |  |
| UserCreate 170 | 95 `user.create` | `MgmtText` | `src/runtime.c` `mgmt_w_user_create` | `ps5upload-core/src/users.rs` |  |
| UserDelete 172 | 96 `user.delete` | `MgmtText` | `src/runtime.c` `mgmt_w_user_delete` | `ps5upload-core/src/users.rs` |  |
| FocusProbe 174 | 57 `proc.focus` | `MgmtText` | `src/runtime.c` `handle_focus_probe` | `ps5upload-core/src/focus.rs` |  |
| BackupSnapshot 176 | 20 `job.run (op BACKUP_SNAPSHOT)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_backup_snapshot` | `ps5upload-core/src/backup.rs` | `ps5upload-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it`; engine `mgmt_job.rs` `a_job_the_console_forgot_is_started_again_except_a_snapshot` |
| BackupList 178 | 98 `backup.list` | `MgmtText` | `src/runtime.c` `mgmt_w_backup_list` | `ps5upload-core/src/backup.rs` |  |
| BackupRestore 180 | 20 `job.run (op BACKUP_RESTORE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | `src/runtime.c` `handle_backup_restore` | `ps5upload-core/src/backup.rs` | `ps5upload-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it` (also runs the restore op); engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` |
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
| AppIconSet 255 | 142 `app.icon_set` | `MgmtText` | `src/runtime.c` `handle_app_icon_set` | `ps5upload-core/src/diagnostics.rs` | hides or shows a title's home-screen icon (#407) |
| AppHiddenList 257 | 143 `app.hidden_list` | `MgmtText` | `src/runtime.c` `handle_app_hidden_list` | `ps5upload-core/src/diagnostics.rs` |  |
| SaveMount 261 | 145 `save.mount` | `MgmtText` {image} | `src/runtime.c` `handle_save_mount` | `ps5upload-core/src/saves.rs` | opens a save: copies its image under /data, unseals the key through /dev/pfsmgr, mounts it with sceFsMountSaveData at /data/ps5upload/save_mount; an empty image reports what is open |
| SaveUnmount 263 | 146 `save.unmount` | `MgmtText` {} | `src/runtime.c` `handle_save_unmount` | `ps5upload-core/src/saves.rs` | closes the open save and copies the image back over the real one (`written_back`) |
| NetPorts 265 | 147 `net.ports` | `MgmtText` {lan} | `src/runtime.c` `handle_net_ports` (`src/net_ports.c`) | `ps5upload-core/src/diagnostics.rs` | the ports held on the console, found by bind() and close (never a connection); `scope` all / console / lan says where each is reachable from |
| FilesPrefsGet 267 | 148 `files.prefs_get` | `MgmtText` | `src/runtime.c` `handle_files_prefs_get` (`src/files_handoff.c`, `src/files_prefs.c`) | `ps5upload-core/src/console_files.rs` | the console file manager's pinned folders, recent folders and last folder (`/data/ps5upload/files-prefs.json`), as the console page's `/api/files/prefs` answers; `payload/tests/files_handoff_selftest.c` |
| FilesPrefsSet 269 | 149 `files.prefs_set` | `MgmtText` {op: pin / unpin / visit, path} | `src/runtime.c` `handle_files_prefs_set` (`src/files_handoff.c`, `src/files_prefs.c`) | `ps5upload-core/src/console_files.rs` | one change under the prefs lock, answers the new prefs; a full list or a bad path is an error |
| FilesHandoffPut 271 | 150 `files.handoff_put` | `MgmtText` {to: console / pc, path} | `src/runtime.c` `handle_files_handoff_put` (`src/files_handoff.c`) | `ps5upload-core/src/console_files.rs` | Show on TV / Show on PC: one slot per direction in helper memory, replaced by the next put, gone after two minutes |
| FilesHandoffTake 273 | 151 `files.handoff_take` | `MgmtText` {for: console / pc} -> {path, age_ms} or {} | `src/runtime.c` `handle_files_handoff_take` (`src/files_handoff.c`) | `ps5upload-core/src/console_files.rs` | takes (and clears) that direction's handoff |
| MinionTile 275 | 152 `minion.tile` | `MgmtText` {tile?: bool, enabled?: bool} -> {tile, enabled, tile_busy, tile_failed, tile_result} | `src/runtime.c` `handle_minion_tile` (`src/webui_api.c`) | `ps5upload-core/src/console_files.rs` | paired tile visibility; `enabled:false` turns Minion off (tile removed and kept removed, :9180 stopped), `enabled:true` back on; preserves LAN access; older helpers return unknown method, or omit `enabled` |
| UsbEject 259 | 144 `usb.eject` | `MgmtText` | `src/runtime.c` `handle_usb_eject` | `ps5upload-core/src/fs_ops.rs` | unmounts `/mnt/usbN` so the drive can be pulled; plain unmount, or `force` (after a sync) since the shell's own services keep the drive open (EBUSY on FW 13.60) |
| CleanupScan 290 | 160 `cleanup.scan` | `MgmtText` {} -> {scan_id} | `src/runtime.c` `handle_cleanup_scan` (`src/storage_cleanup.c`) | `ps5upload-core/src/cleanup.rs` | starts the console clean-up scan on its own thread |
| CleanupStatus 292 | 161 `cleanup.status` | `MgmtText` {scan_id} -> {state, phase, items[], reserve_bytes, ...} | `src/runtime.c` `handle_cleanup_status` | `ps5upload-core/src/cleanup.rs` | items only once the scan is done; at most 400 |
| CleanupApply 294 | 162 `cleanup.apply` | `MgmtText` {scan_id, item_ids[]} -> {apply_id} | `src/runtime.c` `handle_cleanup_apply` | `ps5upload-core/src/cleanup.rs` | re-checks each item; updates go through sceAppInstUtilAppUnInstallPat under sony_api_lock |
| CleanupApplyStatus 296 | 163 `cleanup.apply_status` | `MgmtText` {apply_id} -> {state, done[], skipped[], failed[], freed_bytes} | `src/runtime.c` `handle_cleanup_apply_status` | `ps5upload-core/src/cleanup.rs` |  |
| CleanupCancel 298 | 164 `cleanup.cancel` | `MgmtText` {id} -> {} | `src/runtime.c` `handle_cleanup_cancel` | `ps5upload-core/src/cleanup.rs` | a scan stops at once, an apply after its current item |
| SmbSharesList 310 | 170 `smb.shares_list` | `MgmtText` {} -> {ok, shares[], max} | `src/runtime.c` `handle_smb_shares_list` (`src/webui_smb.c`) | `ps5upload-core/src/console_shares.rs` | the shares the console page (Files, Library) uses; never a password (has_password) |
| SmbSharesSave 312 | 171 `smb.shares_save` | `MgmtText` {id?, name, server, share, user, password?, domain, start_path} -> {ok, share, dialect} | `src/runtime.c` `handle_smb_shares_save` | `ps5upload-core/src/console_shares.rs` | signs in through the SMB daemon first; no password with an id keeps the saved one |
| SmbSharesRemove 314 | 172 `smb.shares_remove` | `MgmtText` {id} -> {ok} | `src/runtime.c` `handle_smb_shares_remove` | `ps5upload-core/src/console_shares.rs` |  |
| MeshHello 316 | 173 `mesh.hello` | `MgmtText` {name, kind, os, app} -> {you, helper, rev, new, settings}: the caller's device record (made on first sight) | `src/runtime.c` `handle_mesh_hello` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshState 318 | 174 `mesh.state` | `MgmtText` {since, ev} -> {rev, same} or the whole picture: devices (online, activity), locks, events after `ev`, settings; prunes devices unseen for `prune_days` | `src/runtime.c` `handle_mesh_state` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshActivity 320 | 175 `mesh.activity` | `MgmtText` {items: [...]} -> {ok, rev}: the caller's presence (a JSON array, 1.5 KiB) | `src/runtime.c` `handle_mesh_activity` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshLock 322 | 176 `mesh.lock` | `MgmtText` {op: take/release, name, ttl, force, label} -> {ok, got, by?, by_name?, label?, until}; a lock lapses when it runs out or its holder goes offline | `src/runtime.c` `handle_mesh_lock` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshDevice 324 | 177 `mesh.device` | `MgmtText` {op: rename/remove/block/unblock, id, name?} -> {ok, rev}; remove and block forget the key (AVA1 trust store) and end its sessions | `src/runtime.c` `handle_mesh_device` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshSettings 326 | 178 `mesh.settings` | `MgmtText` {require_approval?, prune_days?} -> {ok, settings} | `src/runtime.c` `handle_mesh_settings` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshDocGet 328 | 179 `mesh.doc_get` | `MgmtText` {name} -> {rev, doc} | `src/runtime.c` `handle_mesh_doc_get` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshDocPut 330 | 180 `mesh.doc_put` | `MgmtText` {name, base, doc} -> {saved: true, rev} or {saved: false, rev, doc} (written since `base`) | `src/runtime.c` `handle_mesh_doc_put` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| MeshAnnounce 332 | 181 `mesh.announce` | `MgmtText` {kind, text} -> {ok, rev}: an event every device sees | `src/runtime.c` `handle_mesh_announce` (`src/mesh.c`) | `ps5upload-core/src/mesh.rs` | `payload/tests/mesh_selftest.c` |
| VinputCaps 340 | 185 `vinput.caps` | `MgmtText` {} -> {mouse, keyboard, open, backend, companion?, reason?}: which virtual devices this console can make | `src/runtime.c` `handle_vinput_caps` (`src/vinput.c`) | `ps5upload-core/src/vinput.rs` | `payload/tests/vinput_selftest.c`; the stub (both false, `reason` `vinput_unsupported`) unless switched on (FW 13.60 + `/data/ps5upload/vinput.enable`); then backend `relay` to the `ps5u-vinput` companion (`payload/vinput/`), `companion` idle/starting/running/stopped/unavailable, `reason` `vinput_needs_loader` / `vinput_companion_failed`; `payload/tests/vinput_relay_selftest.c` |
| VinputSession 342 | 186 `vinput.session` | `MgmtText` {op: open/hb/close, id, mouse?, keyboard?} -> {ok, open, mouse, keyboard, err?} | `src/runtime.c` `handle_vinput_session` (`src/vinput.c`) | `ps5upload-core/src/vinput.rs` (`ps5upload-engine/src/vinput_api.rs` drives it) | `id` is the owner's token; another open takes over (the old owner's input let go first); no hb or report for 2 s, a close, or the helper stopping lets go of every key and button and removes the devices; `err` `vinput_starting` while the companion starts (open again shortly; on an ok hb: it is being started again); a refusal travels as the call's error cause (`mgmt_call_text_keep`); `payload/tests/vinput_selftest.c` |
| VinputReport 344 | 187 `vinput.report` | `MgmtText` {id, r: hex of 1-64 16-byte reports} -> {ok, applied, err?} | `src/runtime.c` `handle_vinput_report` (`src/vinput.c`) | `ps5upload-core/src/vinput.rs` | the report layout is in `payload/include/vinput.h` (the page's Remote Play binary kind 8 carries the same 16 bytes); a report is also a heartbeat; `payload/tests/vinput_selftest.c` |
| — | 21 `job.list` | empty -> `JobListResult` | `third_party/ava1/c/ava1_data.c` (the data plane) | none | `ps5upload-ctest/tests/job_run.rs` `repeat_job_run_is_idempotent`, `at_most_eight_operations_run_at_once_and_an_unknown_op_is_refused` (listing); no engine caller yet |
| — | 34 `fs.stat` | `FsPath` -> `FsStat` | native `src/mgmt_fs.c` `mgmt_run_fs_stat` | none | host-tested |
| — | 44 `fs.freespace` | `FsPath` -> `FsFreeSpace{usable, free, total, reserve, dev}`: the room an upload may fill on the drive holding the path (nearest existing ancestor): free less the working margin (1/64th, at most 1 GiB), never the memory admit budget or the hidden allocator pool; the engine's up-front space check (review 015 #02) | native `src/mgmt_fs.c` `mgmt_run_fs_freespace` | `ps5upload-core/src/volumes.rs` `free_space` (`ps5upload-ava1/src/space.rs`) | `ps5upload-ctest/tests/mgmt_fs.rs` `fs_freespace_is_the_post_reserve_figure_of_the_drive_holding_the_path`; engine `ps5upload-ava1/tests/mgmt.rs` |

## Reply sizes (SPEC.md §7.4, PROFILE.md §3)

The RPC reply cap is 256 KiB (`RPC_REPLY_MAX`); a `MgmtText` reply holds at most `RPC_TEXT_MAX` = 262,128 bytes of text
(6 bytes of encoding, 13 with `more`). "Max reply" is the largest reply buffer the handler uses (so the largest reply it
can send). Decisions: **fits** (no change), **pages** (offset/limit and `more`), or a clamp. Exactly
one handler (`app.list`) needs paging; `fs.read` pages by its caller; everything else fits. Ported handlers still
detect truncation (SPEC §7.3, PROFILE.md §4): a buffer smaller than the answer is `ERR_INTERNAL`, never a clipped `ok`.

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
| app.icon_set | <= 1 KiB | fits |
| app.hidden_list | 16 KiB | fits |
| usb.eject | <= 0.3 KiB | fits |
| cleanup.scan | <= 0.1 KiB | fits |
| cleanup.status | 224 KiB (400 items) | fits |
| cleanup.apply | <= 0.1 KiB | fits |
| cleanup.apply_status | 224 KiB | fits |
| cleanup.cancel | <= 0.1 KiB | fits |
| smb.shares_list | <= 64 KiB (16 shares) | fits |
| smb.shares_save, smb.shares_remove | <= 4 KiB | fits |
| files.prefs_get, files.prefs_set | 64 KiB (`FILES_PREFS_FILE_MAX`) | fits |
| mesh.state | <= 200 KiB (`MESH_REPLY_MAX`: 64 devices, 1.5 KiB of presence each) | fits |
| mesh.doc_get, mesh.doc_put | 160 KiB document (`MESH_DOC_MAX`) | fits |
| mesh.hello, mesh.activity, mesh.lock, mesh.device, mesh.settings, mesh.announce | <= 2 KiB | fits |
| files.handoff_put, files.handoff_take | <= 6.2 KiB (`FILES_HANDOFF_OUT_MAX`) | fits |
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
| vinput.caps, vinput.session, vinput.report | <= 0.25 KiB (`VINPUT_OUT_MAX`) | fits |
