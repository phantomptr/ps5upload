# Management methods checklist (FTX2 :9114 -> AVA1 :9120)

One row per FTX2 request frame the payload dispatches (`payload/src/runtime.c`, the `hdr.frame_type ==`
chain starting at line 15876), with the AVA1 method that replaces it, the payload handler and the
core/engine caller. `status` goes `todo` -> `ported` (host-tested) -> `hw-verified: <console>`; a row is
marked `hw-verified` only after a run on a real console (Task 22). Rows with method `(none)` are
retired, not ported. Method numbers and bodies are normative in `schema/ava1.toml` and SPEC.md §7.3.

Generated from the tree at the Task 1 commit; line numbers drift, re-find with
`git grep -n "FrameType::<Frame>"` and `git grep -n "FTX2_FRAME_<FRAME>" payload/src/runtime.c`.
`lab` is `ps5upload-lab`, a developer tool deleted with FTX2; "+N test/bench" counts uses in
`ps5upload-tests` and `ps5upload-bench`, which are replaced, not ported.

| frame | method | body | status | payload handler | engine/core caller |
|---|---|---|---|---|---|
| Hello 1 | 1 `node.info` | empty -> `NodeInfo` (exists) | ported (host-tested): `ava1-ctest/tests/server.rs` `rust_client_talks_to_the_c_server` (`node_info`, oversize body) | inline in dispatch (runtime.c:15978); dispatch `runtime.c:15978` | lab `ps5upload-lab/src/main.rs:143`; +4 test/bench |
| BeginTx 10 | (none) | deleted: transfer pipeline (Task 18/19) | n/a | `src/runtime.c:14449` `handle_begin_tx_frame`; dispatch `runtime.c:15884` | `ps5upload-core/src/transfer.rs:549`, `ps5upload-core/src/transfer.rs:1237`, `ps5upload-core/src/transfer.rs:1378`; lab `ps5upload-lab/src/main.rs:210`; +8 test/bench |
| QueryTx 12 | (none) | deleted: transfer pipeline | n/a | `src/runtime.c:15142` `handle_query_tx_frame`; dispatch `runtime.c:15999` | lab `ps5upload-lab/src/main.rs:220`; +1 test/bench |
| CommitTx 14 | (none) | deleted: transfer pipeline | n/a | `src/runtime.c:15227` `handle_commit_tx_frame`; dispatch `runtime.c:16004` | `ps5upload-core/src/transfer.rs:618`; lab `ps5upload-lab/src/main.rs:230`; +3 test/bench |
| AbortTx 16 | (none) | deleted: transfer pipeline | n/a | `src/runtime.c:15735` `handle_abort_tx_frame`; dispatch `runtime.c:16009` | `ps5upload-core/src/transfer.rs:1127`; lab `ps5upload-lab/src/main.rs:240`; +3 test/bench |
| TakeoverRequest 18 | (none) | retired: takeover (Task 8). Kept only in `payload/src/legacy_takeover.c` (sends it to an older helper); AVA1-era instances use the takeover flag file | n/a | inline in dispatch (runtime.c:16014); dispatch `runtime.c:16014` | lab `ps5upload-lab/src/main.rs:195` |
| Status 20 | 4 `node.status` | empty -> `NodeStatus` | ported (host-tested): `ava1-ctest/tests/mgmt.rs` `c_mgmt_node_status_is_typed`; engine `ps5upload-ava1/tests/mgmt.rs` `a_typed_node_status_rebuilds_the_legacy_json_with_a_bool_and_replaced` | `src/runtime.c:15049` `handle_status_frame`; dispatch `runtime.c:15994` | `ps5upload-core/src/health.rs:246`, `ps5upload-engine/src/lib.rs:4757`; lab `ps5upload-lab/src/main.rs:151`; +3 test/bench |
| Shutdown 22 | 5 `node.shutdown` | empty -> empty (the reply is written first, the exit starts 300 ms later on a deferred thread, then `ava1_payload_stop`) | ported | inline in dispatch (runtime.c:16023); dispatch `runtime.c:16023` | `ps5upload-core/src/payload_lifecycle.rs:101`; lab `ps5upload-lab/src/main.rs:171` |
| StreamShard 30 | (none) | deleted: transfer pipeline | n/a | `src/runtime.c:5040` `handle_stream_shard`; dispatch `runtime.c:15876` | `ps5upload-core/src/transfer.rs:742`, `ps5upload-core/src/transfer.rs:977`, `ps5upload-core/src/transfer.rs:981`; lab `ps5upload-lab/src/main.rs:456`; +3 test/bench |
| Cleanup 32 | 6 `node.cleanup` | `MgmtText`; a long tree runs as `job.run (op CLEANUP)` (payload op, Task 5) | ported (host-tested; Task 4) | `src/runtime.c:5543` `handle_cleanup`; dispatch `runtime.c:16031` | `ps5upload-core/src/cleanup.rs:56` |
| FsListVolumes 34 | 32 `fs.volumes` | `MgmtText` | ported (host-tested; Task 4) | `src/runtime.c:5901` `handle_fs_list_volumes`; dispatch `runtime.c:16037` | `ps5upload-core/src/volumes.rs:211`; +1 test/bench |
| FsListDir 36 | 33 `fs.list` | `FsList` -> `FsListResult` (<= 256 entries per call, `more`) | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_list` (path policy hooked from runtime.c) | `ps5upload-core/src/fs_ops.rs:105` |
| FsHash 38 | 20 `job.run (op HASH)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `hash_result_rides_in_status_ext`; engine `ps5upload-ava1/tests/mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` | `src/runtime.c:6319` `handle_fs_hash`; dispatch `runtime.c:16048` | `ps5upload-core/src/fs_ops.rs:159` |
| FsDelete 40 | 20 `job.run (op DELETE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `cancel_stops_a_delete_midway_and_the_job_is_collected_after`; engine `mgmt_job.rs` `fs_delete_over_ava1_is_a_job_and_a_cancel_is_the_word_cancelled` | `src/runtime.c:7500` `handle_fs_delete`; dispatch `runtime.c:16054` | `ps5upload-core/src/fs_ops.rs:335` |
| FsMove 42 | 36 `fs.rename` | `FsRename` -> empty; `ERR_CROSS_DEVICE` | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_rename` (path policy hooked from runtime.c) | `ps5upload-core/src/fs_ops.rs:876` |
| FsChmod 44 | 37 `fs.chmod` | `FsChmod` -> empty (recursive: `job.run` CHMOD_R) | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_chmod` (path policy hooked from runtime.c) | `ps5upload-core/src/fs_ops.rs:911`; +1 test/bench |
| FsMkdir 46 | 35 `fs.mkdir` | `FsMkdir` -> empty | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_mkdir` (path policy hooked from runtime.c) | `ps5upload-core/src/fs_ops.rs:926` |
| FsRead 48 | 38 `fs.read` | `FsRead` -> `FsReadResult` (len <= FS_READ_MAX, loop on `eof`) | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_read` (path policy hooked from runtime.c) | `ps5upload-core/src/download.rs:90`, `ps5upload-core/src/fs_ops.rs:221` |
| FsCopy 50 | 16 `job.copy` | `JobCopy` (exists) | ported (host-tested): `ava1-ctest/tests/copy.rs` `a_folder_copies_on_the_console`, `a_move_deletes_the_source_only_after_a_verified_copy`; engine `ps5upload-ava1/tests/download_copy.rs` `a_console_copy_polls_status_until_done` | `src/runtime.c:7918` `handle_fs_copy`; dispatch `runtime.c:16074` | `ps5upload-core/src/fs_ops.rs:424` |
| FsMount 52 | 40 `fs.mount` | `MgmtText` | ported (host-tested; Task 4) | `src/runtime.c:8571` `handle_fs_mount`; dispatch `runtime.c:16084` | `ps5upload-core/src/fs_ops.rs:822` |
| FsUnmount 54 | 41 `fs.unmount` | `MgmtText` | ported (host-tested; Task 4) | `src/runtime.c:9027` `handle_fs_unmount`; dispatch `runtime.c:16088` | `ps5upload-core/src/fs_ops.rs:848` |
| AppRegister 56 | 48 `app.register` | `MgmtText` | ported | `src/runtime.c:9164` `handle_app_register`; dispatch `runtime.c:16094` | `ps5upload-core/src/fs_ops.rs:969`; +1 test/bench |
| AppUnregister 58 | 49 `app.unregister` | `MgmtText` | ported | `src/runtime.c:9244` `handle_app_unregister`; dispatch `runtime.c:16098` | `ps5upload-core/src/fs_ops.rs:1023`; +1 test/bench |
| AppLaunch 60 | 50 `app.launch` | `MgmtText` | ported | `src/runtime.c:9295` `handle_app_launch`; dispatch `runtime.c:16102` | `ps5upload-core/src/fs_ops.rs:1060`; +1 test/bench |
| AppListRegistered 62 | 51 `app.list` | `MgmtText` (pages: offset/limit, `more`) | ported | `src/runtime.c:9343` `handle_app_list_registered`; dispatch `runtime.c:16106` | `ps5upload-core/src/fs_ops.rs:1099`; +1 test/bench |
| HwInfo 64 | 72 `hw.info` | `MgmtText` | ported | `src/runtime.c:9403` `handle_hw_info`; dispatch `runtime.c:16115` | `ps5upload-core/src/hw.rs:168`; +1 test/bench |
| HwTemps 66 | 73 `hw.temps` | `MgmtText` | ported | `src/runtime.c:9413` `handle_hw_temps`; dispatch `runtime.c:16118` | `ps5upload-core/src/hw.rs:265`; +1 test/bench |
| HwPower 68 | 74 `hw.power` | `MgmtText` | ported | `src/runtime.c:9451` `handle_hw_power`; dispatch `runtime.c:16122` | `ps5upload-core/src/hw.rs:311`; +1 test/bench |
| AppLaunchBrowser 70 | 52 `app.launch_browser` | `MgmtText` | ported | `src/runtime.c:13858` `handle_app_launch_browser`; dispatch `runtime.c:16110` | `ps5upload-core/src/hw.rs:450`; +1 test/bench |
| HwSetFanThreshold 72 | 76 `hw.fan_threshold` | `MgmtText` | ported | `src/runtime.c:13797` `handle_hw_set_fan_threshold`; dispatch `runtime.c:16128` | `ps5upload-core/src/hw.rs:431`; +1 test/bench |
| ProcList 74 | 58 `proc.list` | `MgmtText` | ported | `src/runtime.c:13877` `handle_proc_list`; dispatch `runtime.c:16153` | `ps5upload-core/src/hw.rs:587`; +1 test/bench |
| FsOpStatus 76 | 17 `job.status` | `JobRef` -> `Status` (exists) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `delete_tree_returns_immediately_and_reports_progress`, `status_survives_a_reconnect`; engine `download_copy.rs` `a_console_copy_polls_status_until_done` | `src/runtime.c:7405` `handle_fs_op_status`; dispatch `runtime.c:16078` | `ps5upload-core/src/fs_ops.rs:481` |
| FsOpCancel 78 | 18 `job.cancel` | `JobRef` -> empty (exists) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `cancel_stops_a_delete_midway_and_the_job_is_collected_after`, `another_device_cannot_see_cancel_or_reuse_an_operation`; engine `download_copy.rs` `a_console_copy_cancel_signals_the_job` | `src/runtime.c:7469` `handle_fs_op_cancel`; dispatch `runtime.c:16081` | `ps5upload-core/src/fs_ops.rs:507` |
| HwStorage 80 | 75 `hw.storage` | `MgmtText` | ported | `src/runtime.c:13783` `handle_hw_storage`; dispatch `runtime.c:16125` | `ps5upload-core/src/hw.rs:370` |
| SystemControl 86 | 80 `power.control` | `MgmtText` | ported | `src/runtime.c:10341` `handle_system_control`; dispatch `runtime.c:16192` | `ps5upload-core/src/system_control.rs:70` |
| PowerTelemetry 88 | 81 `power.telemetry` | `MgmtText` | ported | `src/runtime.c:10442` `handle_power_telemetry`; dispatch `runtime.c:16196` | `ps5upload-core/src/system_control.rs:187` |
| UserList 90 | 94 `user.list` | `MgmtText` | ported | `src/runtime.c:10545` `handle_user_list`; dispatch `runtime.c:16199` | `ps5upload-core/src/users.rs:48` |
| ListSaves 92 | 64 `saves.list` | `MgmtText` | ported | `src/runtime.c:11497` `handle_list_saves`; dispatch `runtime.c:16350` | `ps5upload-core/src/saves.rs:37` |
| ListScreenshots 94 | 65 `shots.list` | `MgmtText` | ported | `src/runtime.c:13714` `handle_list_screenshots`; dispatch `runtime.c:16354` | `ps5upload-core/src/saves.rs:67` |
| IndexStart 96 | 67 `index.start` | `MgmtText` | ported | `src/runtime.c:11917` `handle_index_start`; dispatch `runtime.c:16363` | `ps5upload-core/src/search_index.rs:36` |
| IndexStatus 98 | 68 `index.status` | `MgmtText` | ported | `src/runtime.c:11989` `handle_index_status`; dispatch `runtime.c:16367` | `ps5upload-core/src/search_index.rs:72` |
| SearchIndex 100 | 69 `index.search` | `MgmtText` | ported | `src/runtime.c:12008` `handle_search_index`; dispatch `runtime.c:16370` | `ps5upload-core/src/search_index.rs:114` |
| IndexCancel 102 | 70 `index.cancel` | `MgmtText` | ported | `src/runtime.c:13703` `handle_index_cancel`; dispatch `runtime.c:16374` | `ps5upload-core/src/search_index.rs:131` |
| AppLifecycle 104 | 53 `app.lifecycle` | `MgmtText` | ported | `src/runtime.c:12107` `handle_app_lifecycle`; dispatch `runtime.c:16377` | `ps5upload-core/src/app_lifecycle.rs:60` |
| ToastSend 106 | 125 `toast.send` | `MgmtText` | ported | `src/runtime.c:13661` `handle_toast_send`; dispatch `runtime.c:15889` | `ps5upload-core/src/app_lifecycle.rs:136` |
| KlogRead 108 | 7 `log.klog` | `MgmtText` {max_bytes} -> text | ported | `src/runtime.c:12260` `handle_klog_read`; dispatch `runtime.c:16381` | `ps5upload-core/src/diagnostics.rs:24` |
| NetInterfaces 110 | 9 `net.interfaces` | `MgmtText` | ported | `src/runtime.c:12308` `handle_net_interfaces`; dispatch `runtime.c:16385` | `ps5upload-core/src/diagnostics.rs:60` |
| PeripheralControl 112 | 86 `periph.control` | `MgmtText` | ported | `src/runtime.c:12501` `handle_peripheral_control`; dispatch `runtime.c:16388` | `ps5upload-core/src/diagnostics.rs:118` |
| ProcModules 114 | 61 `proc.modules` | `MgmtText` | ported | `src/runtime.c:13597` `handle_proc_modules`; dispatch `runtime.c:16392` | `ps5upload-core/src/diagnostics.rs:642` |
| ShellExec 116 | 87 `shell.exec` | `MgmtText` | ported | `src/runtime.c:12705` `handle_shell_exec`; dispatch `runtime.c:16396` | `ps5upload-core/src/diagnostics.rs:193`; +1 test/bench |
| Crc32File 118 | 20 `job.run (op CRC32)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `crc32_of_a_large_file_is_a_job_that_can_be_cancelled`; engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` | `src/runtime.c:12843` `handle_crc32_file`; dispatch `runtime.c:16400` | `ps5upload-core/src/diagnostics.rs:224` |
| AppDbQuery 120 | 56 `app.db_query` | `MgmtText` | ported | `src/runtime.c:12918` `handle_appdb_query`; dispatch `runtime.c:16404` | `ps5upload-core/src/diagnostics.rs:265` |
| NetSpeedTest 122 | 11 `net.speedtest` | `MgmtText` | ported | `src/runtime.c:13581` `handle_net_speed_test`; dispatch `runtime.c:16415` | `ps5upload-core/src/diagnostics.rs:579` |
| PkgDirectMount 124 | 42 `fs.mount_pkg` | `MgmtText` | ported (host-tested; Task 4) | `src/runtime.c:13115` `handle_pkg_direct_mount`; dispatch `runtime.c:16423` | `ps5upload-core/src/diagnostics.rs:399` |
| UfsFsck 126 | 20 `job.run (op FSCK)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `fsck_keeps_its_ok_false_body_as_the_answer_and_a_frame_error_is_a_failure`; engine `mgmt_job.rs` (same shapes test) | `src/runtime.c:13174` `handle_ufs_fsck`; dispatch `runtime.c:16427` | `ps5upload-core/src/diagnostics.rs:548` |
| LwfsMount 128 | 43 `fs.mount_lwfs` | `MgmtText` | ported (host-tested; Task 4) | `src/runtime.c:13254` `handle_lwfs_mount`; dispatch `runtime.c:16431` | `ps5upload-core/src/diagnostics.rs:516` |
| FsWriteBytes 130 | 39 `fs.write` | `FsWrite` -> empty (chunks <= FSW_CHUNK_MAX, SPEC §7.5) | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_write` (path policy hooked from runtime.c) | `ps5upload-core/src/diagnostics.rs:463` |
| TimeGet 132 | 82 `time.get` | `MgmtText` | ported | `src/runtime.c:9495` `handle_time_get`; dispatch `runtime.c:16132` | `ps5upload-core/src/sys_time.rs:310` |
| TimeSet 134 | 83 `time.set` | `MgmtText` | ported | `src/runtime.c:9528` `handle_time_set`; dispatch `runtime.c:16135` | `ps5upload-core/src/sys_time.rs:341` |
| TimeStateGet 136 | 84 `time.state_get` | `MgmtText` | ported | `src/runtime.c:9631` `handle_time_state_get`; dispatch `runtime.c:16139` | `ps5upload-core/src/sys_time.rs:599` |
| TimeStateSet 138 | 85 `time.state_set` | `MgmtText` | ported | `src/runtime.c:9792` `handle_time_state_set`; dispatch `runtime.c:16142` | `ps5upload-core/src/sys_time.rs:621` |
| SmpMetaControl 140 | 112 `smp.meta_control` | `MgmtText` | ported | `src/runtime.c:9890` `handle_smp_meta_control`; dispatch `runtime.c:16146` | `ps5upload-core/src/smp_meta.rs:68` |
| SmpMetaStats 142 | 113 `smp.meta_stats` | `MgmtText` | ported | `src/runtime.c:10010` `handle_smp_meta_stats`; dispatch `runtime.c:16150` | `ps5upload-core/src/smp_meta.rs:85` |
| SyslogTail 144 | 8 `log.syslog` | `MgmtText` {max_bytes} -> text | ported | `src/runtime.c:14020` `handle_syslog_tail`; dispatch `runtime.c:16167` | `ps5upload-core/src/hw.rs:651` |
| NetReach 148 | 10 `net.reach` | `MgmtText` | ported | `src/runtime.c:13504` `handle_net_reach`; dispatch `runtime.c:16419` | `ps5upload-core/src/diagnostics.rs:629` |
| ProfileInfo 150 | 88 `profile.info` | `MgmtText` | ported | `src/runtime.c:14119` `handle_profile_info`; dispatch `runtime.c:16170` | `ps5upload-core/src/profile.rs:506` |
| ProfileSetUsername 152 | 89 `profile.set_username` | `MgmtText` | ported | `src/runtime.c:14259` `handle_profile_set_username`; dispatch `runtime.c:16173` | `ps5upload-core/src/profile.rs:528` |
| ProfileActivate 154 | 90 `profile.activate` | `MgmtText` | ported | `src/runtime.c:14282` `handle_profile_activate`; dispatch `runtime.c:16177` | `ps5upload-core/src/profile.rs:586` |
| ProfileApplyAvatar 156 | 91 `profile.apply_avatar` | `MgmtText` | ported | `src/runtime.c:14330` `handle_profile_apply_avatar`; dispatch `runtime.c:16180` | `ps5upload-core/src/profile.rs:671` |
| ProfileClearSlot 158 | 92 `profile.clear_slot` | `MgmtText` | ported | `src/runtime.c:14313` `handle_profile_clear_slot`; dispatch `runtime.c:16184` | `ps5upload-core/src/profile.rs:603` |
| ProfileSetLocalUsername 160 | 93 `profile.set_local_username` | `MgmtText` | ported | `src/runtime.c:14413` `handle_profile_set_local_username`; dispatch `runtime.c:16188` | `ps5upload-core/src/profile.rs:560` |
| ProcessList 162 | 59 `proc.process_list` | `MgmtText` | ported | `src/runtime.c:13916` `handle_process_list`; dispatch `runtime.c:16157` | `ps5upload-core/src/process_mgr.rs:90` |
| ProcessKill 164 | 60 `proc.kill` | `MgmtText` | ported | `src/runtime.c:13977` `handle_process_kill`; dispatch `runtime.c:16163` | `ps5upload-core/src/process_mgr.rs:117` |
| ListVideos 166 | 66 `videos.list` | `MgmtText` | ported | `src/runtime.c:13756` `handle_list_videos`; dispatch `runtime.c:16357` | `ps5upload-core/src/saves.rs:89` |
| HwDriveSensors 168 | 79 `hw.drive_sensors` | `MgmtText` | ported | `src/runtime.c:13788` `handle_hw_drive_sensors`; dispatch `runtime.c:16360` | `ps5upload-core/src/hw.rs:538` |
| UserCreate 170 | 95 `user.create` | `MgmtText` | ported | `src/runtime.c:10601` `handle_user_create`; dispatch `runtime.c:16202` | `ps5upload-core/src/users.rs:79` |
| UserDelete 172 | 96 `user.delete` | `MgmtText` | ported | `src/runtime.c:10644` `handle_user_delete`; dispatch `runtime.c:16206` | `ps5upload-core/src/users.rs:116` |
| FocusProbe 174 | 57 `proc.focus` | `MgmtText` | ported | `src/runtime.c:13953` `handle_focus_probe`; dispatch `runtime.c:16160` | `ps5upload-core/src/focus.rs:143` |
| BackupSnapshot 176 | 20 `job.run (op BACKUP_SNAPSHOT)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it`; engine `mgmt_job.rs` `a_job_the_console_forgot_is_started_again_except_a_snapshot` | `src/runtime.c:10691` `handle_backup_snapshot`; dispatch `runtime.c:16210` | `ps5upload-core/src/backup.rs:83` |
| BackupList 178 | 98 `backup.list` | `MgmtText` | ported | `src/runtime.c:10722` `handle_backup_list`; dispatch `runtime.c:16214` | `ps5upload-core/src/backup.rs:112` |
| BackupRestore 180 | 20 `job.run (op BACKUP_RESTORE)` | `JobRun` -> `Status` (ext `state`, `result`, `code`) | ported (host-tested): `ava1-ctest/tests/job_run.rs` `backup_snapshot_reports_progress_and_cancel_stops_it` (also runs the restore op); engine `mgmt_job.rs` `recursive_chmod_hash_crc32_fsck_and_backup_run_as_jobs_with_their_reply_shapes` | `src/runtime.c:10744` `handle_backup_restore`; dispatch `runtime.c:16218` | `ps5upload-core/src/backup.rs:132` |
| BackupDelete 182 | 100 `backup.delete` | `MgmtText` | ported | `src/runtime.c:10770` `handle_backup_delete`; dispatch `runtime.c:16222` | `ps5upload-core/src/backup.rs:162` |
| RemotePlayRequest 188 | 136 `rp.request` | `MgmtText` | ported | `src/runtime.c:10796` `handle_remoteplay_request`; dispatch `runtime.c:16226` | `ps5upload-core/src/remoteplay.rs:30` |
| RemotePlayStatus 189 | 137 `rp.status` | `MgmtText` | ported | `src/runtime.c:10826` `handle_remoteplay_status`; dispatch `runtime.c:16269` | `ps5upload-core/src/remoteplay.rs:76`, `ps5upload-core/src/remoteplay.rs:85` |
| RemotePlayCancel 190 | 138 `rp.cancel` | `MgmtText` | ported | `src/runtime.c:10843` `handle_remoteplay_cancel`; dispatch `runtime.c:16272` | `ps5upload-core/src/remoteplay.rs:94` |
| ActivityGet 192 | 126 `activity.get` | `MgmtText` | ported | `src/runtime.c:11124` `handle_activity_get`; dispatch `runtime.c:16313` | `ps5upload-core/src/activity.rs:88` |
| ActivityDbQuery 194 | 127 `activity.db_query` | `MgmtText` | ported | `src/runtime.c:11142` `handle_activity_db_query`; dispatch `runtime.c:16316` | `ps5upload-core/src/activity.rs:99` |
| HwFanCurveSet 196 | 77 `hw.fan_curve_set` | `MgmtText` | ported | `src/runtime.c:10856` `handle_fan_curve_set`; dispatch `runtime.c:16275` | `ps5upload-core/src/fan_curve.rs:43` |
| NotifList 198 | 122 `notif.list` | `MgmtText` | ported | `src/runtime.c:10873` `handle_notif_list`; dispatch `runtime.c:16278` | `ps5upload-core/src/notif.rs:67` |
| CheatsList 200 | 104 `cheats.list` | `MgmtText` | ported | `src/runtime.c:10973` `handle_cheats_list`; dispatch `runtime.c:16291` | `ps5upload-core/src/cheats.rs:130` |
| CheatsGet 202 | 105 `cheats.get` | `MgmtText` | ported | `src/runtime.c:10991` `handle_cheats_get`; dispatch `runtime.c:16294` | `ps5upload-core/src/cheats.rs:138` |
| CheatsToggle 204 | 106 `cheats.toggle` | `MgmtText` | ported | `src/runtime.c:11019` `handle_cheats_toggle`; dispatch `runtime.c:16297` | `ps5upload-core/src/cheats.rs:158` |
| CheatsDelete 206 | 107 `cheats.delete` | `MgmtText` | ported | `src/runtime.c:11066` `handle_cheats_delete`; dispatch `runtime.c:16300` | `ps5upload-core/src/cheats.rs:169` |
| CheatsReload 208 | 108 `cheats.reload` | `MgmtText` | ported | `src/runtime.c:11080` `handle_cheats_reload`; dispatch `runtime.c:16303` | `ps5upload-core/src/cheats.rs:180` |
| CheatsStatus 210 | 109 `cheats.status` | `MgmtText` | ported | `src/runtime.c:11091` `handle_cheats_status`; dispatch `runtime.c:16306` | `ps5upload-core/src/cheats.rs:191` |
| CheatsEngineSet 212 | 110 `cheats.engine_set` | `MgmtText` | ported | `src/runtime.c:11102` `handle_cheats_engine_set`; dispatch `runtime.c:16309` | `ps5upload-core/src/cheats.rs:202` |
| SdkScan 214 | 114 `sdk.scan` | `MgmtText`; also `job.run (op SDK_SCAN)` (payload op, Task 5) | ported | `src/runtime.c:11171` `handle_sdk_scan`; dispatch `runtime.c:16320` | `ps5upload-core/src/sdk_changer.rs:151` |
| SdkPatch 216 | 115 `sdk.patch` | `MgmtText` | ported | `src/runtime.c:11189` `handle_sdk_patch`; dispatch `runtime.c:16323` | `ps5upload-core/src/sdk_changer.rs:168` |
| SdkRestore 218 | 116 `sdk.restore` | `MgmtText` | ported | `src/runtime.c:11240` `handle_sdk_restore`; dispatch `runtime.c:16326` | `ps5upload-core/src/sdk_changer.rs:198` |
| TmdbFetch 222 | 117 `tmdb.fetch` | `MgmtText` | ported | `src/runtime.c:11284` `handle_tmdb_fetch`; dispatch `runtime.c:16330` | `ps5upload-core/src/tmdb.rs:364` |
| FtpStart 224 | 119 `ftp.start` | `MgmtText` | ported | `src/runtime.c:11356` `handle_ftp_start`; dispatch `runtime.c:16341` | `ps5upload-core/src/ftp.rs:76` |
| FtpStatus 226 | 120 `ftp.status` | `MgmtText` | ported | `src/runtime.c:11384` `handle_ftp_status`; dispatch `runtime.c:16344` | `ps5upload-core/src/ftp.rs:84` |
| TmdbStore 228 | 118 `tmdb.store` | `MgmtText` | ported | `src/runtime.c:11313` `handle_tmdb_store`; dispatch `runtime.c:16333` | `ps5upload-core/src/tmdb.rs:332` |
| FwSpoofStatus 232 | 121 `fwspoof.status` | `MgmtText` | ported | `src/runtime.c:11344` `handle_fw_spoof_status`; dispatch `runtime.c:16337` | `ps5upload-core/src/fw_spoof.rs:54` |
| AppInfoQuery 234 | 54 `app.info_query` | `MgmtText` | ported | `src/runtime.c:12957` `handle_appinfo_query`; dispatch `runtime.c:16407` | `ps5upload-core/src/diagnostics.rs:312` |
| AppInfoSet 236 | 55 `app.info_set` | `MgmtText` | ported | `src/runtime.c:13003` `handle_appinfo_set`; dispatch `runtime.c:16411` | `ps5upload-core/src/diagnostics.rs:348` |
| NotifSend 240 | 123 `notif.send` | `MgmtText` | ported | `src/runtime.c:10935` `handle_notif_send`; dispatch `runtime.c:16287` | none |
| HwFanCurveGet 246 | 78 `hw.fan_curve_get` | `MgmtText` | ported | `src/runtime.c:11396` `handle_fan_curve_get`; dispatch `runtime.c:16347` | `ps5upload-core/src/fan_curve.rs:71` |
| RemotePlayReadiness 248 | 139 `rp.readiness` | `MgmtText` | ported | inline in dispatch (runtime.c:16229, remoteplay_readiness_json); dispatch `runtime.c:16229` | `ps5upload-core/src/remoteplay.rs:218`, `ps5upload-core/src/remoteplay.rs:227` |
| RemotePlayEnable 249 | 140 `rp.enable` | `MgmtText` | ported | inline in dispatch (runtime.c:16239, remoteplay_enable); dispatch `runtime.c:16239` | `ps5upload-core/src/remoteplay.rs:240`, `ps5upload-core/src/remoteplay.rs:249` |
| RemotePlayDevices 250 | 141 `rp.devices` | `MgmtText` | ported | inline in dispatch (runtime.c:16259, remoteplay_devices_json); dispatch `runtime.c:16259` | `ps5upload-core/src/remoteplay.rs:258`, `ps5upload-core/src/remoteplay.rs:267` |
| NotifClear 251 | 124 `notif.clear` | `MgmtText` | ported | `src/runtime.c:10915` `handle_notif_clear`; dispatch `runtime.c:16284` | `ps5upload-core/src/notif.rs:49` |
| ActivityReset 253 | 128 `activity.reset` | `MgmtText` | ported | `src/runtime.c:10895` `handle_activity_reset`; dispatch `runtime.c:16281` | `ps5upload-core/src/activity.rs:125` |
| (raw :9114 probe) | none | TCP connect only (no frame) | ported (host-tested): discovery (`client/src-tauri/src/commands/discover.rs`) now probes the AVA1 port :9120 as well as :9114 (`payload_port_open`), so an AVA1-only payload is found; `a_payload_is_seen_on_either_of_its_ports`. `probes.rs` and the engine takeover paths still use :9114 until FTX2 is removed (Tasks 17-19) | owned by Task 8 | `client/src-tauri/src/commands/discover.rs:80, 414, 559`, `client/src-tauri/src/commands/probes.rs:22, 273` probe the management port directly; they move to the AVA1 port |
| (new) | 21 `job.list` | empty -> `JobListResult` | ported (host-tested): `ava1-ctest/tests/job_run.rs` `repeat_job_run_is_idempotent`, `at_most_eight_operations_run_at_once_and_an_unknown_op_is_refused` (listing); no engine caller yet | owned by Task 5 | none |
| (new) | 34 `fs.stat` | `FsPath` -> `FsStat`; replaces the 1-byte `FsRead` existence test (`ps5upload-engine/src/lib.rs:4033`) | ported (host-tested; Task 4) | native `src/mgmt_fs.c` `mgmt_run_fs_stat` (path policy hooked from runtime.c) | none |
| (new) | 44 `fs.freespace` | `FsPath` -> `FsFreeSpace{usable, free, total, reserve, dev}`: the room an upload may fill on the drive holding the path (nearest existing ancestor): free less the working margin (1/64th, at most 1 GiB), never the memory admit budget or the hidden allocator pool; the engine's up-front space check (review 015 #02) | ported (host-tested): `ava1-ctest/tests/mgmt_fs.rs` `fs_freespace_is_the_post_reserve_figure_of_the_drive_holding_the_path`; engine `ps5upload-ava1/tests/mgmt.rs` | native `src/mgmt_fs.c` `mgmt_run_fs_freespace` | `ps5upload-core/src/volumes.rs` `free_space` (`ps5upload-ava1/src/space.rs`) |

## Reply sizes (SPEC.md §7.4)

The RPC reply cap is 256 KiB (`RPC_REPLY_MAX`); a `MgmtText` reply holds at most `RPC_TEXT_MAX` = 262,128 bytes of text
(6 bytes of encoding, 13 with `more`). "Today's max" is the largest reply buffer the FTX2 handler uses (so the
largest reply it can send). Decisions: **fits** (no change), **pages** (offset/limit and `more`), or a clamp. Exactly
one handler (`app.list`) needs paging; `fs.read` pages by its caller; everything else fits. Ported handlers still
detect truncation (SPEC §7.3): a buffer smaller than the answer is `ERR_INTERNAL`, never a clipped `ok`.

| method | today's max reply | decision |
|---|---|---|
| node.status | ~0.7 KiB (`NodeStatus`) | fits |
| node.cleanup | <= 0.3 KiB | fits |
| log.klog | <= 64 KiB (clamped `max_bytes`) | fits; if it ever did not, the tail rule below applies |
| log.syslog | up to 1 MiB (`HARD_CAP`, runtime.c:14024) | **clamped tail** (`mgmt_call_tail`): the reply is the newest `RPC_TEXT_MAX` bytes, cut at a line start (else a UTF-8 boundary), `more` = 1 when older text was left out; the Rust transport leads such a reply with a one-line note |
| net.interfaces | < 4 KiB (16 interfaces) | fits |
| net.reach, net.speedtest | <= 0.3 KiB | fits |
| fs.volumes | 16 KiB (`RESP_CAP`, runtime.c:5903) | fits |
| fs.list | 32 KiB, <= 256 entries (runtime.c:6113) | fits (`FsListResult`, `more` past 256 entries) |
| fs.mount, fs.unmount, fs.mount_pkg, fs.mount_lwfs | <= 2 KiB (runtime.c:8587, 13165, 13298) | fits |
| fs.read | up to 2 MiB per call (`FS_READ_MAX_BYTES`, runtime.c:601) | pages by the caller: `len <= FS_READ_MAX` (262,128), loop on `eof` |
| fs.write, fs.mkdir, fs.rename, fs.chmod | <= 0.2 KiB | fits |
| app.register, app.unregister, app.launch, app.launch_browser, app.lifecycle | <= 1 KiB | fits |
| app.list | 512 KiB buffer, ~3,800 entries (runtime.c:9343) | **pages with offset/more**: the one handler that does not fit 256 KiB (typical consoles: tens of KiB) |
| app.info_query | 32 KiB (runtime.c:12976) | fits |
| app.info_set | <= 1 KiB | fits |
| app.db_query | 64 KiB (runtime.c:12923) | fits |
| proc.focus | 8 KiB | fits |
| proc.list | 64 KiB (runtime.c:13877) | fits |
| proc.process_list | 256 KiB buffer (runtime.c:13916) | fits when the buffer is sized `RPC_TEXT_MAX` (the JSON must be 16 bytes shorter than the legacy buffer) |
| proc.kill | <= 0.2 KiB | fits |
| proc.modules | 64 KiB (runtime.c:13614) | fits |
| saves.list, shots.list, videos.list | 512 KiB each (`LIST_JSON_CAP`, was 64 KiB) | **pages with offset/more** (the engine merges the pages); a walk that fills the buffer closes with `"truncated":true` instead of looking complete |
| index.start, index.status, index.cancel | <= 1 KiB | fits |
| index.search | 256 KiB buffer, up to 5000 hits (runtime.c:12052) | fits (the handler stops 2.3 KiB short of its buffer, under `RPC_TEXT_MAX`); hits past it are dropped as before and the reply now says `"truncated":true`. Not paged: the request's own `limit` is the hit count |
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
| backup.list | 32 KiB (runtime.c:10727) | fits |
| backup.delete | <= 0.2 KiB | fits |
| cheats.list, cheats.get | 64 KiB (`CHEATS_JSON_BUF_SZ`, runtime.c:10965) | fits |
| cheats.toggle, .delete, .reload, .status, .engine_set | <= 0.5 KiB | fits |
| smp.meta_control, smp.meta_stats | <= 0.4 KiB | fits |
| sdk.scan | 64 KiB (`SDK_JSON_BUF_SZ`, runtime.c:11169) | fits |
| sdk.patch, sdk.restore | <= 1 KiB | fits |
| tmdb.fetch, tmdb.store | 64 KiB (`TMDB_JSON_BUF_SZ`, runtime.c:11282) | fits |
| ftp.start, ftp.status, fwspoof.status | <= 1 KiB | fits |
| notif.list | 32 KiB (runtime.c:10877) | fits |
| notif.send, notif.clear, toast.send | <= 0.1 KiB | fits |
| activity.get, activity.db_query | 128 KiB (`ACTIVITY_JSON_BUF_SZ`, runtime.c:11122) | fits |
| activity.reset | <= 0.1 KiB | fits |
| rp.request, rp.status, rp.cancel, rp.readiness, rp.enable, rp.devices | <= 2 KiB | fits |

Counts: 113 request frames dispatched: 5 retired transfer frames (BeginTx, QueryTx, CommitTx, AbortTx,
StreamShard), 1 retired takeover frame, and 4 that map onto methods that already exist (`Hello` ->
`node.info`, `FsCopy` -> `job.copy`, `FsOpStatus` -> `job.status`, `FsOpCancel` -> `job.cancel`); 7 map onto
`job.run` ops. Two new methods (`job.list`, `fs.stat`) have no frame. `ApplyProgress` (146) and `PkgInstall*`
exist in the `FrameType` enum but the payload does not dispatch them and no engine code sends them.
