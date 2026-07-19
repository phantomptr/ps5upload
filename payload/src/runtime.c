#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/utsname.h>
#include <dlfcn.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/param.h>
#include <sys/mount.h>
#include <sys/sysctl.h>
#include <sys/mdioctl.h>
#include <sys/ioctl.h>
#include <sys/uio.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <net/if_dl.h>
#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <fts.h>
#include <fnmatch.h>
#include <regex.h>
#include "config.h"
#include "runtime.h"
#include "register.h"
#include "hw_info.h"
#include "drive_sensors.h"
#include "backup.h"
#include "remoteplay.h"
#include "fan_curve.h"
#include "notif.h"
#include "sys_time.h"
#include "sys_registry.h"
#include "profile.h"
#include "sony_api_lock.h"
#include "proc_list.h"
#include "smp_meta.h"
#include "blake3.h"

/* PS5 SDK's `<fcntl.h>` hides `posix_fadvise` and its POSIX_FADV_* constants
 * behind `__POSIX_VISIBLE >= 200112`, but defining `_POSIX_C_SOURCE` to unlock
 * them also strips BSD-only types (`struct timeval`, `u_int`, etc.) that the
 * rest of this translation unit needs. Work around that by declaring the
 * prototype + constants ourselves — the underlying syscalls are wired up in
 * libc either way, so this is a visibility fix, not an ABI change. */
#ifndef POSIX_FADV_SEQUENTIAL
#define POSIX_FADV_NORMAL     0
#define POSIX_FADV_RANDOM     1
#define POSIX_FADV_SEQUENTIAL 2
#define POSIX_FADV_WILLNEED   3
#define POSIX_FADV_DONTNEED   4
#define POSIX_FADV_NOREUSE    5
extern int posix_fadvise(int fd, off_t offset, off_t len, int advice);
#endif

/* `posix_fallocate` is hidden behind the same __POSIX_VISIBLE gate as
 * posix_fadvise above. The libc symbol is present (FreeBSD 11+); we
 * just need a visible prototype. Used by cp_rf to reserve contiguous
 * dst extents up-front, which prevents block-allocation interleaving
 * with the data-write loop on multi-GiB copies. */
extern int posix_fallocate(int fd, off_t offset, off_t len);

#define FTX2_MAGIC 0x32585446u
#define FTX2_VERSION 1u
/* NOTE: These frame numbers, MAGIC, VERSION, and the two TX_FLAG_*
 * values below are duplicated in Rust (engine/crates/ftx2-proto/src/lib.rs)
 * and exercised by the payload_c_wire_constants_match test. Keep them
 * in sync or the host and payload will not understand each other. */
#define FTX2_FRAME_HELLO 1u
#define FTX2_FRAME_HELLO_ACK 2u
#define FTX2_FRAME_ERROR 3u
#define FTX2_FRAME_BEGIN_TX 10u
#define FTX2_FRAME_BEGIN_TX_ACK 11u
/* BeginTx TxMeta flag bits (mirror of ftx2-proto's lib.rs FrameType
 * doc comments — the standalone specs/ tree was retired). Bit 0 = RESUME:
 * host asks the payload to reuse an already-interrupted tx_id instead of
 * allocating a fresh slot. Unknown / non-resumable tx_ids fall through to
 * fresh-BeginTx semantics (flag becomes a no-op). */
#define FTX2_TX_FLAG_RESUME 0x1u
/* BeginTx opt-in: client wants APPLY_PROGRESS frames during the
 * multi-file COMMIT_TX apply loop. New in protocol version paired with
 * ftx2_proto::TX_FLAG_APPLY_PROGRESS_REQUESTED. When set, we emit a
 * lightweight JSON progress frame every ~1 sec or 1024 files committed
 * so the desktop client can render a live "Finalized N of M" counter
 * instead of an indeterminate "Finalizing on PS5…" black box. Old
 * clients don't set the flag and we behave as before (silent apply,
 * single CommitTxAck).
 * (See also the cross-language parity test in ftx2-proto.) */
#define FTX2_TX_FLAG_APPLY_PROGRESS_REQUESTED 0x4u
/* (See payload_c_wire_constants_match test in Rust ftx2-proto for the
 * required matching values on the host side.) */
#define FTX2_FRAME_QUERY_TX 12u
#define FTX2_FRAME_QUERY_TX_ACK 13u
#define FTX2_FRAME_COMMIT_TX 14u
#define FTX2_FRAME_COMMIT_TX_ACK 15u
#define FTX2_FRAME_ABORT_TX 16u
#define FTX2_FRAME_ABORT_TX_ACK 17u
#define FTX2_FRAME_TAKEOVER_REQUEST 18u
#define FTX2_FRAME_TAKEOVER_ACK 19u
#define FTX2_FRAME_STATUS 20u
#define FTX2_FRAME_STATUS_ACK 21u
#define FTX2_FRAME_SHUTDOWN 22u
#define FTX2_FRAME_SHUTDOWN_ACK 23u
#define FTX2_FRAME_STREAM_SHARD 30u
#define FTX2_FRAME_SHARD_ACK 31u
#define FTX2_FRAME_CLEANUP 32u
#define FTX2_FRAME_CLEANUP_ACK 33u
#define FTX2_FRAME_FS_LIST_VOLUMES 34u
#define FTX2_FRAME_FS_LIST_VOLUMES_ACK 35u
#define FTX2_FRAME_FS_LIST_DIR 36u
#define FTX2_FRAME_FS_LIST_DIR_ACK 37u
#define FTX2_FRAME_FS_HASH 38u
#define FTX2_FRAME_FS_HASH_ACK 39u
#define FTX2_FRAME_FS_DELETE 40u
#define FTX2_FRAME_FS_DELETE_ACK 41u
#define FTX2_FRAME_FS_MOVE 42u
#define FTX2_FRAME_FS_MOVE_ACK 43u
#define FTX2_FRAME_FS_CHMOD 44u
#define FTX2_FRAME_FS_CHMOD_ACK 45u
#define FTX2_FRAME_FS_MKDIR 46u
#define FTX2_FRAME_FS_MKDIR_ACK 47u
#define FTX2_FRAME_FS_READ 48u
#define FTX2_FRAME_FS_READ_ACK 49u
#define FTX2_FRAME_FS_COPY 50u
#define FTX2_FRAME_FS_COPY_ACK 51u
#define FTX2_FRAME_FS_MOUNT 52u
#define FTX2_FRAME_FS_MOUNT_ACK 53u
#define FTX2_FRAME_FS_UNMOUNT 54u
#define FTX2_FRAME_FS_UNMOUNT_ACK 55u
/* App lifecycle frames — registration + launch. See register.h for
 * the pipeline. Body is JSON:
 *   APP_REGISTER:       {"src_path":"/abs/..."}
 *   APP_UNREGISTER:     {"title_id":"PPSA00xxx"}
 *   APP_LAUNCH:         {"title_id":"PPSA00xxx"}
 *   APP_LIST_REGISTERED: {} (empty)
 * ACK bodies:
 *   APP_REGISTER_ACK:    {"title_id":"...","title_name":"...","used_nullfs":true}
 *   APP_UNREGISTER_ACK:  {} (empty)
 *   APP_LAUNCH_ACK:      {} (empty)
 *   APP_LIST_REGISTERED_ACK: {"apps":[{"title_id":"...","title_name":"...",
 *                                      "src":"/...","image_backed":bool}, ...]}
 */
#define FTX2_FRAME_APP_REGISTER         56u
#define FTX2_FRAME_APP_REGISTER_ACK     57u
#define FTX2_FRAME_APP_UNREGISTER       58u
#define FTX2_FRAME_APP_UNREGISTER_ACK   59u
#define FTX2_FRAME_APP_LAUNCH           60u
#define FTX2_FRAME_APP_LAUNCH_ACK       61u
#define FTX2_FRAME_APP_LIST_REGISTERED     62u
#define FTX2_FRAME_APP_LIST_REGISTERED_ACK 63u
/* Hardware monitoring frames. Bodies are newline-separated "key=value"
 * text (not JSON) — keeps the payload parser trivial. */
#define FTX2_FRAME_HW_INFO          64u
#define FTX2_FRAME_HW_INFO_ACK      65u
#define FTX2_FRAME_HW_TEMPS         66u
#define FTX2_FRAME_HW_TEMPS_ACK     67u
#define FTX2_FRAME_HW_POWER         68u
#define FTX2_FRAME_HW_POWER_ACK     69u
/* Open the PS5 web browser (useful for self-hosted payload-loader
 * pages). Fire-and-forget: the ACK is just a success signal. */
#define FTX2_FRAME_APP_LAUNCH_BROWSER     70u
#define FTX2_FRAME_APP_LAUNCH_BROWSER_ACK 71u
/* Set fan turbo-threshold °C via /dev/icc_fan. Request body is
 * ASCII decimal digits (e.g. "65"); ACK body is empty. The payload
 * clamps to the safe range defined in hw_info.h regardless of input. */
#define FTX2_FRAME_HW_SET_FAN_THRESHOLD     72u
#define FTX2_FRAME_HW_SET_FAN_THRESHOLD_ACK 73u
#define FTX2_FRAME_HW_STORAGE               80u
#define FTX2_FRAME_HW_STORAGE_ACK           81u
/* Walk kernel allproc and return {"ok":bool,"procs":[{"pid":N,"name":"..."},...]}.
 * Body is JSON (unlike the hw_* text bodies above) because the UI wants
 * to render a table of entries, which is nicer with structured parsing
 * than a line-based format. */
#define FTX2_FRAME_PROC_LIST      74u
#define FTX2_FRAME_PROC_LIST_ACK  75u
/* Long-running fs op progress + cancel. The engine sends FS_OP_STATUS
 * (or FS_OP_CANCEL) with a body of `{"op_id":<u64>}` where op_id is
 * the trace_id of the originating FS_COPY/FS_MOVE frame. The payload
 * looks up the matching in-flight op in g_fs_ops[] and replies with
 * its current bytes_copied/total_bytes (status) or sets the cancel
 * flag (cancel). Both work over the same mgmt-port socket pool as
 * other FS_* frames; the engine opens a separate connection so the
 * status poll runs concurrently with the FS_COPY's own connection
 * waiting for FS_COPY_ACK. */
#define FTX2_FRAME_FS_OP_STATUS       76u
#define FTX2_FRAME_FS_OP_STATUS_ACK   77u
#define FTX2_FRAME_FS_OP_CANCEL       78u
#define FTX2_FRAME_FS_OP_CANCEL_ACK   79u
/* Install a `.pkg` file via Sony's BGFT service. Body is JSON
 * `{"url":"http://...","content_id":"...","size":N,"title":"...",
 *   "package_type":"PS4GD"}`. Payload calls into bgft.c which loads
 * libSceBgft.sprx, registers the task, kicks off the download, and
 * returns the BGFT task_id. The host then polls PKG_INSTALL_STATUS
 * for progress + final outcome. ACK body is JSON. */
#define FTX2_FRAME_PKG_INSTALL              82u
#define FTX2_FRAME_PKG_INSTALL_ACK          83u
#define FTX2_FRAME_PKG_INSTALL_STATUS       84u
#define FTX2_FRAME_PKG_INSTALL_STATUS_ACK   85u
/* Power control: reboot/shutdown/standby/tick. Body is a tiny JSON
 * `{"action":"..."}`. Sony API calls inside the payload — no kernel
 * R/W needed. ACK body is `{"ok":true}` or `{"ok":false,"err":"..."}`. */
#define FTX2_FRAME_SYSTEM_CONTROL           86u
#define FTX2_FRAME_SYSTEM_CONTROL_ACK       87u
/* Extended power telemetry — operating seconds, boot count, thermal
 * alert flags. Read-only, ICC-based; safe even without kernel R/W. */
#define FTX2_FRAME_POWER_TELEMETRY          88u
#define FTX2_FRAME_POWER_TELEMETRY_ACK      89u
/* User account enumeration. */
#define FTX2_FRAME_USER_LIST                90u
#define FTX2_FRAME_USER_LIST_ACK            91u
/* Save data listing per-user. */
#define FTX2_FRAME_LIST_SAVES               92u
#define FTX2_FRAME_LIST_SAVES_ACK           93u
/* Screenshot listing. */
#define FTX2_FRAME_LIST_SCREENSHOTS         94u
#define FTX2_FRAME_LIST_SCREENSHOTS_ACK     95u
/* Filesystem index build/query/search. */
#define FTX2_FRAME_INDEX_START              96u
#define FTX2_FRAME_INDEX_START_ACK          97u
#define FTX2_FRAME_INDEX_STATUS             98u
#define FTX2_FRAME_INDEX_STATUS_ACK         99u
#define FTX2_FRAME_SEARCH_INDEX             100u
#define FTX2_FRAME_SEARCH_INDEX_ACK         101u
#define FTX2_FRAME_INDEX_CANCEL             102u
#define FTX2_FRAME_INDEX_CANCEL_ACK         103u
/* App lifecycle (suspend/resume/kill/list). */
#define FTX2_FRAME_APP_LIFECYCLE            104u
#define FTX2_FRAME_APP_LIFECYCLE_ACK        105u
/* Rich JSON toast (sceNotificationSend). */
#define FTX2_FRAME_TOAST_SEND               106u
#define FTX2_FRAME_TOAST_SEND_ACK           107u
/* Kernel log streaming. */
#define FTX2_FRAME_KLOG_READ                108u
#define FTX2_FRAME_KLOG_READ_ACK            109u
/* Network interface enumeration. */
#define FTX2_FRAME_NET_INTERFACES           110u
#define FTX2_FRAME_NET_INTERFACES_ACK       111u
/* Peripheral (BD/USB) control. */
#define FTX2_FRAME_PERIPHERAL_CONTROL       112u
#define FTX2_FRAME_PERIPHERAL_CONTROL_ACK   113u
/* Process module list (loaded sprx). */
#define FTX2_FRAME_PROC_MODULES             114u
#define FTX2_FRAME_PROC_MODULES_ACK         115u
/* Shell command execution. */
#define FTX2_FRAME_SHELL_EXEC               116u
#define FTX2_FRAME_SHELL_EXEC_ACK           117u
/* CRC32 file checksum. */
#define FTX2_FRAME_CRC32_FILE               118u
#define FTX2_FRAME_CRC32_FILE_ACK           119u
/* sqlite query of app.db. */
#define FTX2_FRAME_APPDB_QUERY              120u
#define FTX2_FRAME_APPDB_QUERY_ACK          121u
/* Network round-trip + throughput test. */
#define FTX2_FRAME_NET_SPEED_TEST           122u
#define FTX2_FRAME_NET_SPEED_TEST_ACK       123u
/* Direct .pkg mount via sceFsMountGamePkg. */
#define FTX2_FRAME_PKG_DIRECT_MOUNT         124u
#define FTX2_FRAME_PKG_DIRECT_MOUNT_ACK     125u
/* UFS fsck. */
#define FTX2_FRAME_UFS_FSCK                 126u
#define FTX2_FRAME_UFS_FSCK_ACK             127u
/* LWFS patch overlay mount. */
#define FTX2_FRAME_LWFS_MOUNT               128u
#define FTX2_FRAME_LWFS_MOUNT_ACK           129u
/* Atomic small-file write (≤256 KB). */
#define FTX2_FRAME_FS_WRITE_BYTES           130u
#define FTX2_FRAME_FS_WRITE_BYTES_ACK       131u
/* System clock get/set via sceSystemServiceGet/SetCurrentDateTime
 * (see payload/src/sys_time.c). Bodies are JSON.
 *   TIME_GET request:  {} (empty)
 *   TIME_GET_ACK:      {"ok":bool,"err_code":N,"year":Y,"month":M,
 *                       "day":D,"hour":h,"min":m,"sec":s}
 *   TIME_SET request:  {"year":Y,"month":M,"day":D,"hour":h,"min":m,
 *                       "sec":s}  (UTC; microsecond is unused)
 *   TIME_SET_ACK:      {"ok":bool,"err_code":N,"prior_unix":N,
 *                       "new_unix":N}
 *      prior_unix / new_unix are -1 if get failed; the desktop uses
 *      them to detect "rc=0 but the clock didn't actually move" SDK-
 *      stub no-ops on firmwares where libSceSystemService exports the
 *      symbol but the runtime SPRX doesn't implement it. Requires
 *      ucred-elevated loader (kstuff or similar) since the underlying
 *      SceShellCore IPC checks the caller's system-service capability;
 *      a non-elevated payload sees a Sony privilege-rejected err_code
 *      and surfaces it to the user. */
#define FTX2_FRAME_TIME_GET                 132u
#define FTX2_FRAME_TIME_GET_ACK             133u
#define FTX2_FRAME_TIME_SET                 134u
#define FTX2_FRAME_TIME_SET_ACK             135u
#define FTX2_FRAME_TIME_STATE_GET           136u
#define FTX2_FRAME_TIME_STATE_GET_ACK       137u
#define FTX2_FRAME_TIME_STATE_SET           138u
#define FTX2_FRAME_TIME_STATE_SET_ACK       139u
#define FTX2_FRAME_SMP_META_CONTROL         140u
#define FTX2_FRAME_SMP_META_CONTROL_ACK     141u
#define FTX2_FRAME_SMP_META_STATS           142u
#define FTX2_FRAME_SMP_META_STATS_ACK       143u
/* Recent PS5 kernel log (dmesg equivalent). Body: empty. Ack body:
 * raw text from sysctl kern.msgbuf — the kernel circular buffer of
 * recent printf/printk output. Helps diagnose "payload didn't load /
 * something silently failed" cases from the desktop. */
#define FTX2_FRAME_SYSLOG_TAIL              144u
#define FTX2_FRAME_SYSLOG_TAIL_ACK          145u
/* One-way payload → client during the multi-file COMMIT_TX apply loop
 * when the client opted in via FTX2_TX_FLAG_APPLY_PROGRESS_REQUESTED.
 * Body is a small JSON object (~80 bytes):
 *   {"files_applied":N,"total_files":M,"bytes_applied":B}
 * No ack — fire-and-forget. The client's CommitTxAck wait loop reads
 * and forwards these to the engine's progress sink while it waits for
 * the eventual CommitTxAck. */
#define FTX2_FRAME_APPLY_PROGRESS           146u
/* Profile: avatar image + offline-account (username) operations.
 *   PROFILE_INFO         req {} → ack {"ok":bool,"uid":N,"uid_hex":"0x..",
 *                        "username":"..","slots":[{"slot":N,"name":"..",
 *                        "type":"..","flags":N,"id":"0x..","activated":bool}]}
 *   PROFILE_SET_USERNAME req {"slot":N,"name":".."} → ack {"ok":bool,...}
 *   PROFILE_ACTIVATE     req {"slot":N,"id"?:".."}  → ack {"ok":bool,...}
 *   PROFILE_APPLY_AVATAR req {"uid":N}              → ack {"ok":bool,
 *                        "copied":N} — copies the host-staged DDS/json
 *                        files from /data/ps5upload/profile/0x<UID>/ into
 *                        the live profile cache dir (privileged).
 *   PROFILE_CLEAR_SLOT   req {"slot":N}             → ack {"ok":bool,...}
 * Registry + /system_data writes need ucred elevation (same envelope as
 * the time/registry frames). */
#define FTX2_FRAME_PROFILE_INFO             150u
#define FTX2_FRAME_PROFILE_INFO_ACK         151u
#define FTX2_FRAME_PROFILE_SET_USERNAME     152u
#define FTX2_FRAME_PROFILE_SET_USERNAME_ACK 153u
#define FTX2_FRAME_PROFILE_ACTIVATE         154u
#define FTX2_FRAME_PROFILE_ACTIVATE_ACK     155u
#define FTX2_FRAME_PROFILE_APPLY_AVATAR     156u
#define FTX2_FRAME_PROFILE_APPLY_AVATAR_ACK 157u
#define FTX2_FRAME_PROFILE_CLEAR_SLOT       158u
#define FTX2_FRAME_PROFILE_CLEAR_SLOT_ACK   159u
/* Rename a local console user (the active profile's display name) via
 * sceUserServiceSetUserName. req {"uid":N,"name":".."} → ack {"ok":bool,...}.
 * Distinct from PROFILE_SET_USERNAME, which renames an offline-account slot. */
#define FTX2_FRAME_PROFILE_SET_LOCAL_USERNAME     160u
#define FTX2_FRAME_PROFILE_SET_LOCAL_USERNAME_ACK 161u
/* In-app process manager. PROCESS_LIST is the DETAILED enumerate
 * (proc_list_get_json_ex: pid/name/comm/title_id/app_id/memory/threads/kind)
 * — distinct from the older PROC_LIST (74) which feeds the `ps` diagnostic
 * with just pid+name. PROCESS_KILL SIGKILLs a pid: req {"pid":N} → ack
 * {"ok":bool,...}. Restart is the client composing KILL + APP_LAUNCH. */
#define FTX2_FRAME_PROCESS_LIST      162u
#define FTX2_FRAME_PROCESS_LIST_ACK  163u
#define FTX2_FRAME_PROCESS_KILL      164u
#define FTX2_FRAME_PROCESS_KILL_ACK  165u
/* Video-clip listing (parallels screenshot listing at 94/95). */
#define FTX2_FRAME_LIST_VIDEOS       166u
#define FTX2_FRAME_LIST_VIDEOS_ACK   167u
/* Drive SMART / temperature sensors via SCSI LOG SENSE (CAM pass-through
 * on /dev/daN). Request body empty. ACK body: JSON with per-drive info
 * (device, sizeBytes, ident, tempC, tempErr, fs*) and a fixed-storage
 * array (internal SSD + M.2 summaries). Read-only, no elevation needed. */
#define FTX2_FRAME_HW_DRIVE_SENSORS      168u
#define FTX2_FRAME_HW_DRIVE_SENSORS_ACK  169u
#define FTX2_FRAME_USER_CREATE           170u
#define FTX2_FRAME_USER_CREATE_ACK       171u
#define FTX2_FRAME_USER_DELETE           172u
#define FTX2_FRAME_USER_DELETE_ACK       173u
/* v4.0: Tag-based backup snapshots */
#define FTX2_FRAME_BACKUP_SNAPSHOT       176u
#define FTX2_FRAME_BACKUP_SNAPSHOT_ACK   177u
#define FTX2_FRAME_BACKUP_LIST           178u
#define FTX2_FRAME_BACKUP_LIST_ACK       179u
#define FTX2_FRAME_BACKUP_RESTORE        180u
#define FTX2_FRAME_BACKUP_RESTORE_ACK    181u
#define FTX2_FRAME_BACKUP_DELETE         182u
#define FTX2_FRAME_BACKUP_DELETE_ACK     183u
/* v4.1: Remote Play PIN */
#define FTX2_FRAME_REMOTEPLAY_REQUEST    188u
#define FTX2_FRAME_REMOTEPLAY_STATUS     189u
#define FTX2_FRAME_REMOTEPLAY_CANCEL     190u
#define FTX2_FRAME_REMOTEPLAY_CANCEL_ACK 191u
/* v4.1: Fan curve editor (set + get) */
#define FTX2_FRAME_HW_FAN_CURVE_SET      196u
#define FTX2_FRAME_HW_FAN_CURVE_SET_ACK  197u
#define FTX2_FRAME_HW_FAN_CURVE_GET      246u
#define FTX2_FRAME_HW_FAN_CURVE_GET_ACK  247u
/* v4.1: Persistent notifications (list + send) */
#define FTX2_FRAME_NOTIF_LIST            198u
#define FTX2_FRAME_NOTIF_LIST_ACK        199u
#define FTX2_FRAME_NOTIF_SEND            240u
#define FTX2_FRAME_NOTIF_SEND_ACK        241u
/* Frame numbers 184-187, 192-195, 200-239 and 242-245 were allocated during
 * v4 scaffolding for features that were never finished (save resign, activity
 * tracker, cheats, SDK changer, FTP/SMB, TMDB, firmware spoof, Linux/plugin
 * loaders, fpkg-guard, garlic) or were SKIP per FEATURE-GAP-ANALYSIS.md
 * (incl. 224-229: Game Dumper / pkg-zone / PSN Fake Sign-In — piracy/account
 * fraud). They were removed in v4; the numbers are left unallocated so any
 * stale peer that still sends them gets a clean UnknownFrameType error. */
/* Where we place mount points. Scoped under /mnt/ps5upload/ so it
 * never collides with mount paths owned by other utilities. */
#define FS_MOUNT_BASE "/mnt/ps5upload"
#define FS_MOUNT_MD_CTL  "/dev/mdctl"
#define FS_MOUNT_LVD_CTL "/dev/lvdctl"
/* Max wait for /dev/md<N> or /dev/lvd<N> to appear after attach.
 * Device nodes usually show up within a few hundred microseconds,
 * but allow up to 2 s so a slow sandbox doesn't falsely report
 * "attach failed". */
#define FS_MOUNT_DEV_WAIT_RETRIES 200
#define FS_MOUNT_DEV_WAIT_US      10000u  /* 10 ms × 200 = 2 s */

/* ── Sony LVD (Logical Volume Device) ioctl interface ────────────────────
 *
 * Reverse-engineered LVD constants + structs. The PS5 kernel prefers
 * LVD over plain FreeBSD md(4) for file-backed images; MDIOCATTACH
 * often fails on PS5 where LVD succeeds. We try LVD first and fall
 * back to MD if LVD returns an error.
 *
 * Struct layouts are PS5-kernel-specific — DO NOT reorder fields. */

#define FS_MOUNT_LVD_IOC_ATTACH_V0 0xC0286D00ull
#define FS_MOUNT_LVD_IOC_DETACH    0xC0286D01ull

/* LVD attach raw flags → normalized flags (precomputed). Sony's
 * sceFsLvdAttachCommon normalizes a wrapper-side raw bitmask into the
 * value the validator checks; we hardcode the outputs since we only
 * support a fixed set of (fstype, ro) combinations. The rules are:
 *
 *   exfat / pfs (single-image family):  raw 0x8 → 0x14 RW, 0x9 → 0x1C RO
 *   ufs (dd/lwfs family):                raw 0xC → 0x16 RW, 0xD → 0x1E RO */
#define FS_MOUNT_LVD_FLAGS_EXFAT_RW 0x14u
#define FS_MOUNT_LVD_FLAGS_EXFAT_RO 0x1Cu
#define FS_MOUNT_LVD_FLAGS_UFS_RW   0x16u
#define FS_MOUNT_LVD_FLAGS_UFS_RO   0x1Eu
#define FS_MOUNT_LVD_FLAGS_PFS_RW   0x14u  /* PFS uses single-image family */
#define FS_MOUNT_LVD_FLAGS_PFS_RO   0x1Cu
#define FS_MOUNT_LVD_SECONDARY_SINGLE 0x10000u

/* LVD image_type values accepted by the validator (0..0xC). The three
 * we care about: SINGLE for exfat, UFS_DOWNLOAD_DATA for ffpkg, and
 * PFS_SAVE_DATA for ffpfs. */
#define FS_MOUNT_LVD_IMAGE_SINGLE      0u
#define FS_MOUNT_LVD_IMAGE_PFS_SAVE    5u
#define FS_MOUNT_LVD_IMAGE_UFS_DD      7u

/* nmount(2) third-arg flags. PS5's UFS mount path (DD/LWFS images
 * attached via /dev/lvdN) requires the magic 0x10000000 bit set in
 * the flags arg — without it nmount returns EINVAL on every PS5
 * firmware we've tested. exfatfs and pfs take plain MNT_RDONLY/0. */
#define FS_MOUNT_UFS_NMOUNT_FLAG_RW 0x10000000u
#define FS_MOUNT_UFS_NMOUNT_FLAG_RO 0x10000001u

/* PFS option payload defaults. We only ever mount fake-signed PFS
 * images (the kernel's signature/key checks are bypassed by the
 * loader running before this payload), so sigverify=playgo=disc=0
 * and the EKPFS key is the 64-hex-char zero key (PFS images that
 * accept these defaults are the fake-signed family we target).
 * mkeymode=SD selects the SD-card key derivation path. */
#define FS_MOUNT_PFS_SIGVERIFY "0"
#define FS_MOUNT_PFS_PLAYGO    "0"
#define FS_MOUNT_PFS_DISC      "0"
#define FS_MOUNT_PFS_MKEYMODE  "SD"
#define FS_MOUNT_PFS_EKPFS_HEX \
    "0000000000000000000000000000000000000000000000000000000000000000"

/* Source-stability gate. When non-zero, refuses to mount an image
 * whose mtime is newer than this many seconds. Originally a defense
 * against "user mounts mid-upload" but in practice it bites every
 * normal user: ps5upload's COMMIT_TX_ACK already proves the file is
 * whole + fsync'd, and the user clicking Mount right after upload
 * is the *expected* flow. The gate as a 3-second wall produced
 * `fs_mount_source_unstable: image modified 1 s ago` failures on
 * legitimate mounts and forced the user to wait + retry.
 *
 * Set to 0 (disabled) since the COMMIT_TX_ACK is the real
 * stability signal we trust. Other ingest paths (FTP, manual cp,
 * etc.) that lack a clean "I'm done" signal will surface as
 * natural mount errors during the LVD attach / nmount step
 * instead of a misleading "modified 1 s ago" rejection.
 *
 * The constant is kept (vs ripping the whole if-block) so a
 * future build that needs to re-enable a stability heuristic for
 * a specific ingest path can flip it back without protocol
 * changes. */
#define FS_MOUNT_STABILITY_SECONDS 0

typedef struct {
    uint16_t source_type;      /* +0x00: 1 = file, 2 = block/char device */
    uint16_t flags;            /* +0x02: bit0 = NO_BITMAP */
    uint32_t reserved0;        /* +0x04: must be zero */
    const char *path;          /* +0x08: backing file path */
    uint64_t offset;           /* +0x10: offset within backing object */
    uint64_t size;             /* +0x18: exposed size in bytes */
    const char *bitmap_path;   /* +0x20: unused when NO_BITMAP set */
    uint64_t bitmap_offset;    /* +0x28: unused */
    uint64_t bitmap_size;      /* +0x30: unused */
} fs_mount_lvd_layer_t;

typedef struct {
    uint32_t io_version;       /* +0x00: 0 = V0/base */
    int32_t  device_id;        /* +0x04: in=-1 for auto, out=unit assigned */
    uint32_t sector_size;      /* +0x08: exposed sector size (512 or 4096) */
    uint32_t secondary_unit;   /* +0x0C: LVD_SECONDARY_SINGLE for exfat */
    uint16_t flags;            /* +0x10: normalized attach flags */
    uint16_t image_type;       /* +0x12: 0=single, 7=ufs_dd, 5=pfs_save */
    uint32_t layer_count;      /* +0x14: 1 for single-layer images */
    uint64_t device_size;      /* +0x18: total exposed virtual size */
    fs_mount_lvd_layer_t *layers_ptr; /* +0x20: array of layer descriptors */
} fs_mount_lvd_attach_t;

typedef struct {
    uint32_t reserved0;        /* +0x00: must be zero */
    int32_t  device_id;        /* +0x04: unit to detach */
    uint8_t  reserved[0x20];   /* +0x08: kernel ABI padding */
} fs_mount_lvd_detach_t;
/* Hard cap on FS_READ response size. 2 MiB is comfortably above the
 * largest icon0.png we've observed (~700 KiB) while keeping a single
 * ACK well under the payload's malloc ceiling and the send buffer
 * size. Clients asking for more get truncated silently — metadata
 * fetches don't need recovery semantics. */
#define FS_READ_MAX_BYTES (2u * 1024u * 1024u)
#define FTX2_HEADER_LEN 28u
#define FTX2_TX_META_LEN 24u
#define FTX2_SHARD_HEADER_LEN 64u
#define FTX2_SHARD_ACK_LEN 48u
#define FTX2_ACK_STATE_SPOOLED 0u
#define FTX2_ACK_STATE_APPLIED 1u
#define FTX2_ACK_STATE_DUPLICATE_IGNORED 2u
#define FTX2_SHARD_DRAIN_BUF 65536u

/* ShardHeader.flags bit definitions. */
#define FTX2_SHARD_FLAG_PACKED 0x1u
#define FTX2_PACKED_RECORD_PREFIX_LEN 8u   /* u32 path_len + u32 data_len */

typedef struct {
    uint32_t magic;
    uint16_t version;
    uint16_t frame_type;
    uint32_t flags;
    uint64_t body_len;
    uint64_t trace_id;
} ftx2_header_t;

typedef struct {
    unsigned char tx_id[16];
    uint32_t kind;
    uint32_t flags;
} ftx2_tx_meta_t;

/* Shard header prefix (64 bytes, LE) — matches ftx2-proto::ShardHeader */
typedef struct {
    unsigned char tx_id[16];        /* offset  0 */
    uint64_t shard_seq;             /* offset 16 */
    unsigned char shard_digest[32]; /* offset 24 — BLAKE3-256 */
    uint32_t record_count;          /* offset 56 */
    uint32_t flags;                 /* offset 60 */
} ftx2_shard_header_t;             /* 64 bytes */

/* SHARD_ACK body (48 bytes, LE) — matches ftx2-proto::ShardAck */
typedef struct {
    unsigned char tx_id[16];    /* offset  0 */
    uint64_t shard_seq;         /* offset 16 */
    uint8_t ack_state;          /* offset 24 */
    uint8_t pad[7];             /* offset 25 */
    uint64_t bytes_committed;   /* offset 32 */
    uint64_t files_committed;   /* offset 40 */
} ftx2_shard_ack_t;            /* 48 bytes */

/* Forward type declaration — full definition appears later in the file. */
typedef struct manifest_file_entry manifest_file_entry_t;
typedef struct manifest_index_entry manifest_index_entry_t;

/* Forward declarations */
static uint32_t read_le32(const unsigned char *p);
/* Component-scoped `..`/`.` rejection used by both is_path_allowed and
 * cleanup_path_allowed. Defined further down near is_path_allowed. */
static int path_has_dotdot_component(const char *p);
/* Writable-roots allowlist. Used by every destructive FS handler and
 * the BEGIN_TX/STREAM_SHARD ingestion path so a hostile manifest can
 * not write outside the allowlisted roots. See is_path_allowed for
 * the exact set. Also used by backup.c's restore path so a crafted
 * manifest can't write outside the allowlist. Declared in runtime.h. */
int is_path_allowed(const char *p);
/* JSON-escape helper. Used by ACK builders that embed user-controlled
 * paths/strings into JSON bodies. Defined alongside FS_LIST_VOLUMES. */
static size_t json_escape_into(const char *src, char *dst, size_t dst_cap);
static const char *json_string_end(const char *start, const char *limit);
static int json_copy_unescaped_string(const char *start, const char *end,
                                      char *out, size_t out_len);
/* Find the closing brace of the JSON object at `obj_start`, skipping any
 * brace that appears inside a quoted string value (e.g. a file path that
 * literally contains '}'). See the definition for why a plain strchr('}')
 * is wrong here. */
static const char *json_object_end(const char *obj_start, const char *limit);
static const char *find_bounded(const char *hay, size_t hay_len,
                                const char *needle);
/* Forward declarations — runtime_reconcile_mounts uses fs_mount and
 * mount_tracker helpers defined further down in the file. */
static int fs_mount_try_unmount(const char *mount_point);
static int fs_mount_detach_md(int unit_id);
static int fs_mount_detach_lvd(int unit_id);
static int  mount_tracker_read(const char *mount_point, char *out, size_t out_cap);
static void mount_tracker_write(const char *mount_point, const char *src_path);
static void mount_tracker_remove(const char *mount_point);
static int  mount_tracker_exists(const char *mount_point);
static int handle_stream_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, uint64_t body_len);
static int runtime_write_manifest(const runtime_tx_entry_t *entry,
                                   const char *manifest_json, size_t manifest_len);
static int runtime_read_manifest_alloc(const runtime_tx_entry_t *entry,
                                        char **out_buf, size_t *out_len);
static int manifest_get_nth_file_path(const char *json, uint64_t n,
                                       manifest_file_entry_t *out);
static void runtime_release_tx_resources(runtime_tx_entry_t *entry);
/* Variant that preserves the `.ps5up2-tmp` file(s) and manifest heap so a
 * resumed BEGIN_TX can pick up where the interrupted one left off. Used
 * whenever the tx is transitioning to "interrupted" (TCP drop, takeover,
 * shutdown) — NOT for terminal transitions (commit/abort), which use
 * the full-cleanup variant above. */
static void runtime_release_tx_resources_ephemeral(runtime_tx_entry_t *entry);
/* Wall-clock microseconds since the Unix epoch. Defined later in the
 * file; forward-declared here so the takeover/shutdown teardown
 * watchdog in runtime_mark_active_transactions can call it. */
static uint64_t now_us(void);

/* Per-connection tracking for the transfer port: records the tx_id of
 * the currently-open transaction on this connection so the accept loop
 * can mark it "interrupted" when the socket closes without COMMIT/ABORT.
 * Mgmt-port handlers pass NULL — they never open a tx. */
typedef struct {
    unsigned char tx_id[16];
    int           has_tx;  /* 1 between BEGIN_TX_ACK and COMMIT/ABORT_ACK */
} conn_tx_ctx_t;

/* Mark a single tx entry "interrupted" iff it's currently "active". Preserves
 * tmp file(s) + manifest heap so a subsequent BEGIN_TX with TX_FLAG_RESUME
 * can adopt the entry and pick up from the last-acked shard. Safe to call
 * with a tx_id that no longer exists (e.g. the tx committed between the
 * frame returning and the connection closing) — becomes a no-op. */
static void runtime_mark_tx_interrupted_by_id(runtime_state_t *state,
                                                const unsigned char *tx_id);

/* ── Directory helper ───────────────────────────────────────────────────────── */

static int ensure_dir(const char *path) {
    if (!path || !*path) return -1;
    if (mkdir(path, 0777) == 0) return 0;
    if (errno == EEXIST) return 0;
    return -1;
}

/* ── TX summary state (active count + last seq) ─────────────────────────────── */

static int runtime_load_tx_state(runtime_state_t *state) {
    FILE *fp = NULL;
    unsigned long long active = 0;
    unsigned long long last_seq = 0;
    if (!state) return -1;
    fp = fopen(state->tx_state_path, "r");
    if (!fp) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    if (fscanf(fp, "active_transactions=%llu\nlast_tx_seq=%llu\n", &active, &last_seq) == 2) {
        state->active_transactions = (uint64_t)active;
        state->last_tx_seq = (uint64_t)last_seq;
    }
    fclose(fp);
    return 0;
}

static int runtime_save_tx_state(const runtime_state_t *state) {
    FILE *fp = NULL;
    if (!state) return -1;
    fp = fopen(state->tx_state_path, "w");
    if (!fp) return -1;
    fprintf(fp,
            "active_transactions=%llu\nlast_tx_seq=%llu\n",
            (unsigned long long)state->active_transactions,
            (unsigned long long)state->last_tx_seq);
    fclose(fp);
    return 0;
}

/* ── TX event log ────────────────────────────────────────────────────────────── */

static int runtime_append_tx_event(const runtime_state_t *state, const char *event_name) {
    FILE *fp = NULL;
    if (!state || !event_name) return -1;
    fp = fopen(state->tx_journal_path, "a");
    if (!fp) return -1;
    fprintf(fp,
            "ts=%llu event=%s active_transactions=%llu last_tx_seq=%llu\n",
            (unsigned long long)time(NULL),
            event_name,
            (unsigned long long)state->active_transactions,
            (unsigned long long)state->last_tx_seq);
    fclose(fp);
    return 0;
}

/* ── Per-TX shard log ────────────────────────────────────────────────────────── */

/*
 * Per-shard log. Kept as an open FILE* on the tx entry so we pay one fopen()
 * per transaction, not one per shard (which cost ~1 ms × N shards on PS5).
 * Casting away const: the cache is a pure-lazy-init optimisation of an
 * otherwise stateless log append, not an observable state change.
 */
static void runtime_append_shard_log(const runtime_tx_entry_t *entry,
                                      uint64_t shard_seq, uint64_t data_len) {
    runtime_tx_entry_t *mut;
    FILE *fp;
    if (!entry) return;
    mut = (runtime_tx_entry_t *)entry;
    fp = (FILE *)mut->shard_log_fp;
    if (!fp) {
        char path[512];
        snprintf(path, sizeof(path), "%s/shards_%s.log",
                 PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
        fp = fopen(path, "a");
        if (!fp) return;
        mut->shard_log_fp = (void *)fp;
    }
    fprintf(fp, "shard_seq=%llu bytes=%llu ts=%llu\n",
            (unsigned long long)shard_seq,
            (unsigned long long)data_len,
            (unsigned long long)time(NULL));
    /* No fflush here — one fflush on terminal transition (release_tx_resources)
     * is enough. Losing the tail of the log on crash is fine; the log is for
     * post-hoc diagnostics, not recovery. */
}

/* ── JSON helpers ─────────────────────────────────────────────────────────────── */

static void extract_json_string_field(const char *json, const char *field,
                                       char *out, size_t out_len) {
    char needle[64];
    const char *pos = NULL;
    const char *start = NULL;
    const char *end = NULL;
    if (!json || !field || !out || out_len == 0) return;
    out[0] = '\0';
    snprintf(needle, sizeof(needle), "\"%s\":\"", field);
    pos = strstr(json, needle);
    if (!pos) return;
    start = pos + strlen(needle);
    end = json_string_end(start, NULL);
    if (!end) return;
    if (json_copy_unescaped_string(start, end, out, out_len) != 0) out[0] = '\0';
}

static uint64_t extract_json_uint64_field(const char *json, const char *field) {
    char needle[64];
    const char *pos = NULL;
    if (!json || !field) return 0;
    snprintf(needle, sizeof(needle), "\"%s\":", field);
    pos = strstr(json, needle);
    if (!pos) return 0;
    pos += strlen(needle);
    errno = 0;
    uint64_t val = strtoull(pos, NULL, 10);
    /* On overflow, strtoull returns ULLONG_MAX and sets ERANGE.
     * A hostile or buggy manifest could send a 40-digit "file_size";
     * returning ULLONG_MAX would make the payload try to allocate
     * or pre-allocate an absurd amount. Clamp to 0 so the caller's
     * "no value" path is taken instead. */
    if (errno == ERANGE) return 0;
    return val;
}

/* ── TX ID helpers ───────────────────────────────────────────────────────────── */

static void tx_id_bytes_to_hex(const unsigned char *tx_id, char *out, size_t out_len) {
    static const char hex[] = "0123456789abcdef";
    size_t i = 0;
    if (!tx_id || !out || out_len < 33) return;
    for (i = 0; i < 16; i++) {
        out[i * 2]     = hex[(tx_id[i] >> 4) & 0x0f];
        out[i * 2 + 1] = hex[tx_id[i] & 0x0f];
    }
    out[32] = '\0';
}

static int tx_id_bytes_equal(const unsigned char *a, const unsigned char *b) {
    return memcmp(a, b, 16) == 0;
}

static int hex_char_value(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return 10 + (ch - 'a');
    if (ch >= 'A' && ch <= 'F') return 10 + (ch - 'A');
    return -1;
}

static int tx_id_hex_to_bytes(const char *hex, unsigned char *out) {
    int i = 0;
    if (!hex || !out) return -1;
    for (i = 0; i < 16; i++) {
        int hi = hex_char_value(hex[i * 2]);
        int lo = hex_char_value(hex[i * 2 + 1]);
        if (hi < 0 || lo < 0) return -1;
        out[i] = (unsigned char)((hi << 4) | lo);
    }
    return 0;
}

/* ── TX table helpers ─────────────────────────────────────────────────────────── */

/* Per-slot mutex array, parallel to runtime_state_t::tx_entries[]. Held
 * by handlers (SHARD, COMMIT_TX, ABORT_TX) for the duration of any
 * mutation/dereference of the entry, so two handlers can never race on
 * the same slot — most importantly, COMMIT_TX cannot free
 * entry->manifest_index/direct_writer while a STREAM_SHARD on the
 * transfer thread is dereferencing them.
 *
 * Kept in a parallel array (not embedded in runtime_tx_entry_t) so
 * the memset(entry, 0, …) at slot eviction time doesn't clobber the
 * mutex memory. Indexed by `entry - state->tx_entries`. Initialized
 * once from runtime_init via runtime_init_entry_mutexes(); guarded
 * by the singleton init flag because there is only ever one
 * runtime_state_t per process. */
static pthread_mutex_t g_entry_mtx[PS5UPLOAD2_MAX_TX];
static int g_entry_mtx_initialized = 0;

static int runtime_init_entry_mutexes(void) {
    int i;
    if (g_entry_mtx_initialized) return 0;
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (pthread_mutex_init(&g_entry_mtx[i], NULL) != 0) {
            return -1;
        }
    }
    g_entry_mtx_initialized = 1;
    return 0;
}

static int entry_slot_index(const runtime_state_t *state,
                            const runtime_tx_entry_t *entry) {
    return (int)(entry - state->tx_entries);
}

/* Caller must hold state_mtx. Plain table walk — no synchronization
 * of its own. Use runtime_acquire_tx_entry from handler code instead
 * unless you specifically need the unlocked variant (e.g. inside an
 * already-locked critical section). */
static runtime_tx_entry_t *runtime_find_tx_entry(runtime_state_t *state,
                                                   const unsigned char *tx_id) {
    int i = 0;
    if (!state || !tx_id) return NULL;
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (state->tx_entries[i].in_use &&
            tx_id_bytes_equal(state->tx_entries[i].tx_id, tx_id)) {
            return &state->tx_entries[i];
        }
    }
    return NULL;
}

/* Acquire a tx entry for exclusive access by the calling handler.
 *
 * Returns the entry pointer with the per-slot mutex held, or NULL if
 * no such tx exists (or if the slot was evicted before we could
 * acquire it). Caller must pair with runtime_release_tx_entry.
 *
 * Lock discipline: takes state_mtx briefly twice (lookup + revalidate)
 * but never holds state_mtx while waiting for the per-slot mutex —
 * that ordering avoids deadlock against handler code that takes
 * state_mtx for short critical sections (e.g. command_count++) while
 * holding the per-slot mutex.
 *
 * The revalidation step exists because between releasing state_mtx
 * after the initial find and acquiring the per-slot mutex, an
 * eviction running under state_mtx could have repurposed the slot for
 * a different tx_id. Re-finding under state_mtx confirms our pointer
 * still names the requested tx. */
static runtime_tx_entry_t *runtime_acquire_tx_entry(runtime_state_t *state,
                                                      const unsigned char *tx_id) {
    runtime_tx_entry_t *entry = NULL;
    int idx = -1;
    if (!state || !tx_id) return NULL;

    pthread_mutex_lock(&state->state_mtx);
    entry = runtime_find_tx_entry(state, tx_id);
    if (!entry) {
        pthread_mutex_unlock(&state->state_mtx);
        return NULL;
    }
    idx = entry_slot_index(state, entry);
    pthread_mutex_unlock(&state->state_mtx);

    pthread_mutex_lock(&g_entry_mtx[idx]);

    /* Revalidate: another thread may have evicted this slot while we
     * were blocked on the per-slot mutex. */
    pthread_mutex_lock(&state->state_mtx);
    {
        runtime_tx_entry_t *re = runtime_find_tx_entry(state, tx_id);
        if (re != entry) {
            pthread_mutex_unlock(&state->state_mtx);
            pthread_mutex_unlock(&g_entry_mtx[idx]);
            return NULL;
        }
    }
    pthread_mutex_unlock(&state->state_mtx);
    return entry;
}

static void runtime_release_tx_entry(runtime_state_t *state,
                                     runtime_tx_entry_t *entry) {
    int idx;
    if (!state || !entry) return;
    idx = entry_slot_index(state, entry);
    if (idx < 0 || idx >= PS5UPLOAD2_MAX_TX) return;
    pthread_mutex_unlock(&g_entry_mtx[idx]);
}

/* True if `entry` is in a terminal state and its slot can be recycled. */
static int runtime_tx_state_is_terminal(const runtime_tx_entry_t *entry) {
    if (!entry || !entry->in_use) return 1;
    return strcmp(entry->state, "committed")   == 0 ||
           strcmp(entry->state, "aborted")     == 0 ||
           strcmp(entry->state, "interrupted") == 0 ||
           /* "apply_failed" is the new terminal state for COMMIT_TX
            * apply failures (writer I/O error, rename failure,
            * spool-apply failure). Without it here, a slot in
            * apply_failed would never be evicted, and after enough
            * such failures the tx table would saturate at
            * MAX_TX entries and reject all future BEGIN_TX with
            * "tx_table_full". */
           strcmp(entry->state, "apply_failed") == 0;
}

/* Caller must hold state_mtx. Returns an entry slot for `tx_id`,
 * either the existing one (no mutation) or a freshly recycled slot
 * (terminal, no other thread currently using it). Eviction requires
 * pthread_mutex_trylock on the per-slot mutex to succeed — if a
 * handler thread still holds the slot for I/O on a finished tx
 * (e.g. building the COMMIT_TX_ACK body after release_tx_resources),
 * we must not evict it underneath that handler. */
static runtime_tx_entry_t *runtime_alloc_tx_entry(runtime_state_t *state,
                                                    const unsigned char *tx_id,
                                                    uint64_t tx_seq) {
    int i = 0;
    int evict_idx = -1;
    int candidate_idx = -1;
    uint64_t oldest_seq = UINT64_MAX;
    runtime_tx_entry_t *entry = NULL;
    if (!state || !tx_id) return NULL;
    entry = runtime_find_tx_entry(state, tx_id);
    if (entry) return entry;
    /* First pass: any free slot wins. Trylock to confirm the slot is
     * not in the middle of being released by another handler — even
     * with in_use==0 the per-slot mutex may still be held briefly. */
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) {
            if (pthread_mutex_trylock(&g_entry_mtx[i]) == 0) {
                evict_idx = i;
                break;
            }
        }
    }
    /* Second pass: evict the oldest terminal tx (committed/aborted/interrupted)
     * whose per-slot mutex we can take. trylock failure means a handler is
     * still using the slot (e.g. emitting an ACK from post-release fields);
     * leave it alone and try a different one. */
    if (evict_idx < 0) {
        for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
            if (!runtime_tx_state_is_terminal(&state->tx_entries[i])) continue;
            if (state->tx_entries[i].tx_seq >= oldest_seq) continue;
            if (pthread_mutex_trylock(&g_entry_mtx[i]) != 0) continue;
            if (candidate_idx >= 0) {
                /* Found a strictly older candidate — release the previous
                 * one so we don't leak the trylock. */
                pthread_mutex_unlock(&g_entry_mtx[candidate_idx]);
            }
            oldest_seq = state->tx_entries[i].tx_seq;
            candidate_idx = i;
        }
        evict_idx = candidate_idx;
    }
    if (evict_idx < 0) return NULL;
    entry = &state->tx_entries[evict_idx];
    /* If we're reusing a terminal slot, its heap state (manifest blob + index)
     * must be freed before we memset — otherwise we leak both allocations. */
    runtime_release_tx_resources(entry);
    memset(entry, 0, sizeof(*entry));
    entry->in_use = 1;
    memcpy(entry->tx_id, tx_id, 16);
    tx_id_bytes_to_hex(tx_id, entry->tx_id_hex, sizeof(entry->tx_id_hex));
    entry->tx_seq = tx_seq;
    snprintf(entry->state, sizeof(entry->state), "active");
    /* Release the per-slot mutex now that the slot is initialized. The
     * caller (BEGIN_TX) still holds state_mtx, so no other thread can
     * find this slot until BEGIN_TX completes its setup and unlocks. */
    pthread_mutex_unlock(&g_entry_mtx[evict_idx]);
    return entry;
}

/* ── TX record persistence ────────────────────────────────────────────────────── */

/*
 * Write (or overwrite) the JSON record for a transaction from in-memory state.
 * Called on BEGIN_TX, COMMIT_TX, ABORT_TX, and when marking interrupted.
 */
static int runtime_flush_tx_record(const runtime_state_t *state,
                                    const runtime_tx_entry_t *entry) {
    char path[512];
    char dest_root_esc[512];
    FILE *fp = NULL;
    if (!state || !entry) return -1;
    snprintf(path, sizeof(path), "%s/tx_%s.json", PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "w");
    if (!fp) return -1;
    json_escape_into(entry->dest_root, dest_root_esc, sizeof(dest_root_esc));
    fprintf(fp,
            "{\"tx_id\":\"%s\",\"tx_seq\":%llu,\"state\":\"%s\","
            "\"shards_received\":%llu,\"bytes_received\":%llu,"
            "\"total_shards\":%llu,\"total_bytes\":%llu,"
            "\"file_count\":%llu,\"multi_file\":%d,\"dest_root\":\"%s\"}\n",
            entry->tx_id_hex,
            (unsigned long long)entry->tx_seq,
            entry->state,
            (unsigned long long)entry->shards_received,
            (unsigned long long)entry->bytes_received,
            (unsigned long long)entry->total_shards,
            (unsigned long long)entry->total_bytes,
            (unsigned long long)entry->file_count,
            entry->multi_file ? 1 : 0,
            dest_root_esc);
    fclose(fp);
    return 0;
}

/* Read the persisted JSON record for a transaction entry into buf. */
static int runtime_read_tx_record(const runtime_tx_entry_t *entry,
                                   char *buf, size_t buf_len) {
    char path[512];
    FILE *fp = NULL;
    size_t got = 0;
    if (!entry || !buf || buf_len == 0) return -1;
    snprintf(path, sizeof(path), "%s/tx_%s.json", PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "r");
    if (!fp) return -1;
    got = fread(buf, 1, buf_len - 1, fp);
    buf[got] = '\0';
    fclose(fp);
    return 0;
}

/* ── TX table bulk operations ────────────────────────────────────────────────── */

static int runtime_load_tx_entries(runtime_state_t *state) {
    DIR *dir = NULL;
    struct dirent *de = NULL;
    if (!state) return -1;
    dir = opendir(PS5UPLOAD2_TX_DIR);
    if (!dir) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    while ((de = readdir(dir)) != NULL) {
        char tx_id_hex[33];
        unsigned char tx_id[16];
        char path[512];
        char record[1024];
        unsigned long long tx_seq = 0;
        runtime_tx_entry_t *entry = NULL;
        const char *name = de->d_name;
        if (strncmp(name, "tx_", 3) != 0) continue;
        if (strlen(name) < 3 + 32 + 5) continue;
        memcpy(tx_id_hex, name + 3, 32);
        tx_id_hex[32] = '\0';
        if (strcmp(name + 35, ".json") != 0) continue;
        if (tx_id_hex_to_bytes(tx_id_hex, tx_id) != 0) continue;
        snprintf(path, sizeof(path), "%s/%s", PS5UPLOAD2_TX_DIR, name);
        {
            FILE *fp = fopen(path, "r");
            size_t got = 0;
            if (!fp) continue;
            got = fread(record, 1, sizeof(record) - 1, fp);
            record[got] = '\0';
            fclose(fp);
        }
        tx_seq = extract_json_uint64_field(record, "tx_seq");
        if (tx_seq == 0) continue;
        entry = runtime_alloc_tx_entry(state, tx_id, tx_seq);
        if (!entry) continue;
        extract_json_string_field(record, "state", entry->state, sizeof(entry->state));
        if (entry->state[0] == '\0') {
            snprintf(entry->state, sizeof(entry->state), "active");
        }
        entry->shards_received = extract_json_uint64_field(record, "shards_received");
        entry->bytes_received  = extract_json_uint64_field(record, "bytes_received");
        entry->total_shards    = extract_json_uint64_field(record, "total_shards");
        entry->total_bytes     = extract_json_uint64_field(record, "total_bytes");
        entry->file_count      = extract_json_uint64_field(record, "file_count");
        /* Restore the multi-file flag. Legacy journals (written before this
         * field existed) report 0; fall back to the old `file_count > 1`
         * proxy so a pre-upgrade in-flight tx still routes correctly. New
         * journals carry the authoritative value (1 even for a kind==2 tx
         * that happens to hold a single file). */
        entry->multi_file      = (extract_json_uint64_field(record, "multi_file") != 0) ||
                                 (entry->file_count > 1);
        extract_json_string_field(record, "dest_root", entry->dest_root, sizeof(entry->dest_root));
    }
    closedir(dir);
    return 0;
}

static void runtime_reconcile_tx_state(runtime_state_t *state) {
    int i = 0;
    uint64_t active = 0;
    uint64_t last_seq = 0;
    if (!state) return;
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) continue;
        if (strcmp(state->tx_entries[i].state, "active") == 0) active += 1;
        if (state->tx_entries[i].tx_seq > last_seq) last_seq = state->tx_entries[i].tx_seq;
    }
    state->active_transactions = active;
    if (last_seq > state->last_tx_seq) state->last_tx_seq = last_seq;
}

/* Tear down every active transaction. Called from TAKEOVER_REQUEST and
 * SHUTDOWN handlers when a fresh payload instance (or stop) wants to
 * leave the table in a clean state.
 *
 * Two-phase to keep the per-slot mutex acquisition deadlock-free:
 *   1. Snapshot which slot indices are active, under state_mtx.
 *   2. For each, lock the per-slot mutex, re-validate the slot still
 *      names an active tx, then mutate + flush. Acquiring per-slot
 *      mutexes outside state_mtx matches the discipline established
 *      by runtime_acquire_tx_entry (state_mtx → per-slot, never the
 *      reverse) so handler threads holding a per-slot mutex aren't
 *      blocked waiting for state_mtx that we don't hold during the
 *      release. */
static void runtime_mark_active_transactions(runtime_state_t *state, const char *new_state) {
    int active_idx[PS5UPLOAD2_MAX_TX];
    int active_count = 0;
    int i = 0;
    int is_interrupt = 0;
    if (!state || !new_state) return;
    is_interrupt = (strcmp(new_state, "interrupted") == 0);

    pthread_mutex_lock(&state->state_mtx);
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) continue;
        if (strcmp(state->tx_entries[i].state, "active") != 0) continue;
        active_idx[active_count++] = i;
    }
    pthread_mutex_unlock(&state->state_mtx);

    for (i = 0; i < active_count; i++) {
        int slot = active_idx[i];
        runtime_tx_entry_t *entry = &state->tx_entries[slot];
        uint64_t teardown_start_us;
        uint64_t teardown_elapsed_us;
        /* Block until any in-flight SHARD/COMMIT/ABORT on this slot
         * finishes — we cannot free direct_writer / manifest_index
         * while another thread is dereferencing them. We also keep
         * the mutex held across the eventual release_tx_resources*
         * teardown below: dropping it before the teardown's
         * pthread_join would re-open the SHARD/teardown race the
         * per-slot mutex exists to prevent. The cost is that a stuck
         * pack-worker (kernel write blocked on dead storage) stalls
         * shutdown until the syscall returns; the watchdog log below
         * surfaces that case so operators can see the stall instead
         * of guessing why takeover hung. */
        pthread_mutex_lock(&g_entry_mtx[slot]);
        /* Revalidate: state may have flipped to non-active (e.g. a
         * concurrent COMMIT_TX completed) between the snapshot and
         * acquiring this mutex. Skip in that case. */
        if (!entry->in_use || strcmp(entry->state, "active") != 0) {
            pthread_mutex_unlock(&g_entry_mtx[slot]);
            continue;
        }
        snprintf(entry->state, sizeof(entry->state), "%s", new_state);
        (void)runtime_flush_tx_record(state, entry);
        teardown_start_us = now_us();
        /* "interrupted" is a pauseable state — the client may reconnect with
         * TX_FLAG_RESUME and pick up from last_acked_shard. Preserve the
         * tmp file(s) and manifest so that works. For any other transition
         * (currently unused, but keep the behavior defensive), fall through
         * to the full release which unlinks the tmp. */
        if (is_interrupt) {
            runtime_release_tx_resources_ephemeral(entry);
        } else {
            runtime_release_tx_resources(entry);
        }
        teardown_elapsed_us = now_us() - teardown_start_us;
        /* 5-second watchdog: pthread_join inside pack_pool_teardown
         * normally returns in microseconds (workers ack `shutdown=1`
         * on the next queue pop). A multi-second teardown means a
         * worker is stuck in a syscall — likely UFS write blocked on
         * detached storage. Log the slot + elapsed time so the next
         * operator triaging "shutdown took 30s" has the breadcrumb
         * to tell them which tx wedged. */
        if (teardown_elapsed_us > 5ULL * 1000ULL * 1000ULL) {
            fprintf(stderr,
                    "[payload2] WARN: mark_active_transactions teardown of tx %s "
                    "took %llu ms (likely stuck pack-worker syscall)\n",
                    entry->tx_id_hex,
                    (unsigned long long)(teardown_elapsed_us / 1000ULL));
        }
        pthread_mutex_unlock(&g_entry_mtx[slot]);
    }
    pthread_mutex_lock(&state->state_mtx);
    runtime_reconcile_tx_state(state);
    pthread_mutex_unlock(&state->state_mtx);
    (void)runtime_save_tx_state(state);
}

static void runtime_mark_tx_interrupted_by_id(runtime_state_t *state,
                                                const unsigned char *tx_id) {
    /* Three-phase teardown so a stuck pack-worker pthread_join inside
     * `runtime_release_tx_resources_ephemeral` cannot stall the mgmt
     * port:
     *
     *   1. Under state_mtx: find the entry, transition state to
     *      "interrupted", decrement active_transactions, snapshot the
     *      fields we need for the flush, and capture the slot index.
     *      Don't touch heap/thread state under state_mtx — that would
     *      hold the global lock across pthread_join.
     *   2. Under g_entry_mtx[slot] only: drain + tear down the
     *      thread-bound resources (writer thread, pack-worker pool).
     *      Other handlers waiting on the same per-slot mutex (e.g. a
     *      late SHARD on a resumed tx using the same id) will block,
     *      which is the correct behavior — they shouldn't read
     *      manifest_index while we're freeing it.
     *   3. Lock-free: persist the snapshot to the on-disk tx record so
     *      the next BEGIN_TX with TX_FLAG_RESUME finds "interrupted"
     *      and adopts the partial data. */
    runtime_tx_entry_t snapshot;
    int should_flush = 0;
    int slot = -1;
    if (!state || !tx_id) return;

    pthread_mutex_lock(&state->state_mtx);
    {
        runtime_tx_entry_t *entry = runtime_find_tx_entry(state, tx_id);
        if (entry && strcmp(entry->state, "active") == 0) {
            snprintf(entry->state, sizeof(entry->state), "interrupted");
            /* `active_transactions` was bumped at BEGIN_TX and not
             * decremented by commit/abort (which never ran here).
             * Decrement now so the counter reflects reality; the entry
             * survives for resume lookup. */
            if (state->active_transactions > 0) state->active_transactions -= 1;
            slot = entry_slot_index(state, entry);
            snapshot = *entry;  /* copy fields used by the flush below */
            should_flush = 1;
        }
    }
    pthread_mutex_unlock(&state->state_mtx);

    if (slot >= 0) {
        pthread_mutex_lock(&g_entry_mtx[slot]);
        /* Re-validate: between unlocking state_mtx and acquiring the
         * per-slot mutex, runtime_alloc_tx_entry may have evicted this
         * slot (we just transitioned to "interrupted" which is
         * terminal — eligible for eviction). The eviction path already
         * called runtime_release_tx_resources during reuse, so re-doing
         * it here would be safe-but-wasted; tx_id mismatch tells us
         * to skip. */
        runtime_tx_entry_t *entry = &state->tx_entries[slot];
        if (entry->in_use && tx_id_bytes_equal(entry->tx_id, tx_id)) {
            runtime_release_tx_resources_ephemeral(entry);
        }
        pthread_mutex_unlock(&g_entry_mtx[slot]);
    }

    if (should_flush) {
        (void)runtime_flush_tx_record(state, &snapshot);
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "conn_drop_interrupt");
    }
}

/* ── Public lifecycle ─────────────────────────────────────────────────────────── */

int runtime_ensure_directories(void) {
    /* Critical dirs — all under /data which the loader's process
     * always has write access to. If any of these fail, the payload
     * truly can't function, so abort startup. */
    if (ensure_dir(PS5UPLOAD2_RUNTIME_ROOT) != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_RUNTIME_DIR)  != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_TX_DIR)       != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_SPOOL_DIR)    != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_DEBUG_DIR)    != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_MOUNTS_DIR)   != 0) return -1;
    /* Optional dirs under /user — Sony-managed root with stricter
     * permissions. Without ucred elevation (kstuff not loaded yet)
     * these mkdirs fail with EACCES. They're only used by the pkg
     * install flow, so a failure here MUST NOT abort startup —
     * otherwise sending ps5upload before kstuff would prevent the
     * mgmt port from ever opening, breaking the whole "load kstuff
     * later" recovery path the rest of the codebase supports.
     *
     * The pkg-install handler re-tries the mkdirs at request time
     * (after kstuff has had a chance to land), so the user's first
     * pkg install still succeeds even if startup couldn't pre-create
     * the dirs. */
    if (ensure_dir(PS5UPLOAD2_USER_DATA_ROOT) != 0) {
        fprintf(stderr,
                "[payload2] /user/data dir create skipped (likely no kstuff yet); "
                "pkg install will retry on demand\n");
    } else {
        /* Only attempt the leaf if the parent succeeded. */
        if (ensure_dir(PS5UPLOAD2_PKG_TEMP_DIR) != 0) {
            fprintf(stderr,
                    "[payload2] pkg_temp dir create skipped; "
                    "pkg install will retry on demand\n");
        }
    }
    return 0;
}

/* Sweep stale Tier-1 staging files. Called once on payload init,
 * after runtime_ensure_directories. Removes any *.pkg in
 * PS5UPLOAD2_PKG_TEMP_DIR whose mtime is older than 24h — these are
 * orphans from a desktop-side crash mid-install that the engine
 * couldn't clean up. The 24h cutoff avoids racing a legitimate
 * in-flight install from another desktop session.
 *
 * Best-effort: failures (opendir/stat/unlink) are logged-and-skipped.
 * The desktop's normal post-install delete handles the steady-state
 * cleanup; this sweep is purely the crash-recovery safety net. */
void runtime_sweep_stale_pkg_temp(void) {
    DIR *d = opendir(PS5UPLOAD2_PKG_TEMP_DIR);
    if (!d) return;
    time_t cutoff = time(NULL) - (24 * 60 * 60);
    struct dirent *ent;
    int swept = 0;
    while ((ent = readdir(d)) != NULL) {
        if (ent->d_name[0] == '.') continue;
        char path[512];
        snprintf(path, sizeof(path), "%s/%s",
                 PS5UPLOAD2_PKG_TEMP_DIR, ent->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        if (!S_ISREG(st.st_mode)) continue;
        if (st.st_mtime > cutoff) continue;
        if (unlink(path) == 0) swept += 1;
    }
    closedir(d);
    if (swept > 0) {
        fprintf(stderr,
                "[payload2] swept %d stale staging file(s) from %s\n",
                swept, PS5UPLOAD2_PKG_TEMP_DIR);
    }
}

/* Startup reconciliation for `/mnt/ps5upload/` mounts.
 *
 * Walks the kernel mount table. For each of our mounts, validates:
 *   1. The backing dev node (f_mntfromname) still exists.
 *   2. The recorded source image (from the .src tracker) still exists.
 *
 * If either check fails the mount is orphaned — force-unmount +
 * detach + cleanup + remove tracker. Result: every payload startup
 * leaves the Volumes screen showing only valid, usable mounts.
 *
 * Why we need this:
 *   - Mounts survive payload restarts (kernel holds them), so without
 *     reconciliation a "Volumes" screen after a payload re-send shows
 *     old mounts the user didn't do in the current session — confusing.
 *   - If the user deletes the backing .exfat file while it's mounted,
 *     the mount silently breaks. Reconciliation at next payload start
 *     cleans up the dead mount instead of leaving it to surface
 *     misleading errors.
 *
 * Intentionally tolerant: any single failure logs + continues. A
 * reconciliation error never prevents the payload from coming up.
 * At worst, one stale mount stays visible for one more session. */
/* getmntinfo(3) is NOT thread-safe: it returns a pointer to a single
 * libc-owned static buffer that the next call in ANY thread overwrites (and
 * may realloc, invalidating an earlier caller's pointer). The mgmt port runs
 * up to PS5UPLOAD2_MAX_MGMT_THREADS concurrent client threads, several of
 * which call getmntinfo (FS_LIST_VOLUMES, the FS_MOUNT reuse scan, FS_UNMOUNT,
 * and the shell `mount`/`df`/`mtrw` verbs) — so two racing callers can read a
 * half-replaced array or a dangling pointer. Serialize the call and hand back
 * a PRIVATE heap copy the caller frees with free(). The lock is held only
 * around getmntinfo + the memcpy — a tiny, return-free region — never during
 * the caller's iteration, so it can neither deadlock nor serialize the stat()
 * work the callers do. Returns the entry count (0 on failure/empty) and sets
 * *out to a malloc'd array (NULL on failure/empty). EVERY caller must free()
 * the array on every exit path once it's done reading it. */
static int mntinfo_snapshot(struct statfs **out) {
    static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
    *out = NULL;
    struct statfs *copy = NULL;
    int n = 0;
    pthread_mutex_lock(&mtx);
    struct statfs *libc_mnts = NULL;
    int got = getmntinfo(&libc_mnts, MNT_NOWAIT);
    if (got > 0 && libc_mnts != NULL) {
        size_t bytes = (size_t)got * sizeof(struct statfs);
        copy = (struct statfs *)malloc(bytes);
        if (copy != NULL) {
            memcpy(copy, libc_mnts, bytes);
            n = got;
        }
    }
    pthread_mutex_unlock(&mtx);
    *out = copy;
    return n;
}

void runtime_reconcile_mounts(void) {
    struct statfs *mnts = NULL;
    int nmnts = mntinfo_snapshot(&mnts);
    if (nmnts <= 0 || mnts == NULL) return;

    int cleaned = 0;
    int kept    = 0;
    for (int i = 0; i < nmnts; i++) {
        const char *mnt_on   = mnts[i].f_mntonname;
        const char *mnt_from = mnts[i].f_mntfromname;
        /* Reconcile our own mounts only. Two cases:
         *   - Legacy: anything under /mnt/ps5upload/<name>. We always
         *     own these (the namespace is reserved by handle_fs_mount).
         *   - User-chosen mount paths: identified by tracker presence.
         *     A user-mounted /mnt/ext1/games/foo has a tracker at
         *     /data/ps5upload/mounts/mnt_ext1_games_foo.src; system
         *     mounts at /mnt/ext1 itself do not.
         * Skip everything else so we never accidentally unmount a
         * Sony-managed mount or the user's own filesystem. */
        const int legacy_ours =
            strncmp(mnt_on, "/mnt/ps5upload/", 15) == 0 && mnt_on[15] != '\0';
        if (!legacy_ours && !mount_tracker_exists(mnt_on)) continue;

        int orphaned = 0;
        const char *reason = "unknown";

        /* Check the dev node. If it's a /dev/md* or /dev/lvd* that
         * no longer stats, the MDIOCATTACH/LVD entry is gone and
         * the mount can't do anything useful. */
        struct stat dev_st;
        if (stat(mnt_from, &dev_st) != 0) {
            orphaned = 1;
            reason = "dev_node_gone";
        }

        /* Check the source image file. If it was deleted/moved since
         * the mount was created, keep the mount — users may have
         * intentionally moved the file and we don't own cleanup of
         * that. Log only; don't clean up. */
        char src[512];
        int have_src = mount_tracker_read(mnt_on, src, sizeof(src));
        if (have_src) {
            struct stat src_st;
            if (stat(src, &src_st) != 0) {
                /* Source file gone — flag for info, but DON'T unmount.
                 * Filesystem on /dev/lvd* is self-contained; the
                 * source file being missing is a diagnostic, not a
                 * correctness problem. Leaving this mount alive lets
                 * the user finish whatever they were doing. */
                fprintf(stderr,
                    "[payload2] mount %s: source %s missing (keeping mount)\n",
                    mnt_on, src);
            }
        }

        if (orphaned) {
            fprintf(stderr,
                "[payload2] reconcile: unmounting orphan %s (%s)\n",
                mnt_on, reason);
            /* Extract the unit number so we can release the attachment. */
            int lvd_unit = -1, md_unit = -1;
            if (strncmp(mnt_from, "/dev/lvd", 8) == 0 &&
                mnt_from[8] >= '0' && mnt_from[8] <= '9') {
                lvd_unit = atoi(mnt_from + 8);
            } else if (strncmp(mnt_from, "/dev/md", 7) == 0 &&
                       mnt_from[7] >= '0' && mnt_from[7] <= '9') {
                md_unit = atoi(mnt_from + 7);
            }
            (void)fs_mount_try_unmount(mnt_on);
            if (lvd_unit >= 0) (void)fs_mount_detach_lvd(lvd_unit);
            if (md_unit  >= 0) (void)fs_mount_detach_md(md_unit);
            (void)rmdir(mnt_on);
            mount_tracker_remove(mnt_on);
            cleaned += 1;
        } else {
            kept += 1;
        }
    }
    if (cleaned > 0 || kept > 0) {
        fprintf(stderr,
            "[payload2] reconcile: kept %d mount(s), cleaned %d orphan(s)\n",
            kept, cleaned);
    }
    free(mnts);
}

/* Encode a mount_point to a filesystem-safe tracker filename (no
 * extension). Two formats coexist for backward compatibility:
 *
 *   - Legacy /mnt/ps5upload/<name> mounts use the leaf <name> as the
 *     key (matches files written by every payload up through 2.2.24).
 *   - User-chosen mount paths (anywhere `is_path_allowed` accepts —
 *     /mnt/ext1/games/foo, /data/mounts/bar, etc.) hex-escape every
 *     non-alphanumeric byte. So `/mnt/ext1/games/foo` becomes
 *     `mnt_2fext1_2fgames_2ffoo` (the `_2f` triplet is the hex of
 *     '/'; `_5f` would be the hex of a literal underscore).
 *
 * Why hex-escape rather than a flat `/` → `_` substitution: paths
 * `/mnt/ext1/foo_bar` and `/mnt/ext1/foo/bar` would both encode to
 * `mnt_ext1_foo_bar` under a flat substitution — silently
 * colliding. With per-byte hex escaping every distinct mount_point
 * has a distinct key, no matter how many underscores its segments
 * contain. The triplet form (`_HH`) is one byte longer per escaped
 * char but stays well within the 256-byte key buffer for any
 * realistic PS5 path.
 *
 * The legacy format is stable across upgrades — existing PS5s with
 * trackers from earlier versions keep showing source-image strings
 * in the Volumes tab without re-mount. */
static void mount_tracker_key(const char *mount_point, char *out, size_t out_cap) {
    if (!out || out_cap == 0) return;
    out[0] = '\0';
    if (!mount_point) return;
    const size_t base_len = strlen(FS_MOUNT_BASE);
    /* Legacy: /mnt/ps5upload/<name> with non-empty leaf. Use the leaf
     * as the key so trackers written by 2.2.24 and earlier still
     * resolve. The leaf is constrained by handle_fs_mount to contain
     * no slashes / dots, so it's safe to use verbatim. Defensive
     * extra check: if the "leaf" actually contains a slash (a path
     * like /mnt/ps5upload/foo/bar can reach this code via a future
     * caller that bypasses handle_fs_mount's name validation), fall
     * through to the hex-escape branch so the tracker key stays
     * collision-free instead of silently producing `foo/bar.src` —
     * a path-traversal-shaped filename whose stat()/open() then
     * fails on a non-existent intermediate directory. */
    if (strncmp(mount_point, FS_MOUNT_BASE "/", base_len + 1) == 0 &&
        mount_point[base_len + 1] != '\0' &&
        strchr(mount_point + base_len + 1, '/') == NULL) {
        snprintf(out, out_cap, "%s", mount_point + base_len + 1);
        return;
    }
    /* New: hex-escape every non-alphanumeric byte, skipping a single
     * leading slash. Each escaped byte takes 3 chars (`_` + 2 hex
     * digits), so we need 3 bytes of headroom per escaped char plus
     * 1 for the NUL. Stop early if the buffer would overflow rather
     * than truncate mid-escape (a partial `_5` would be ambiguous). */
    static const char HEX[] = "0123456789abcdef";
    size_t i = (mount_point[0] == '/') ? 1u : 0u;
    size_t j = 0;
    while (mount_point[i] != '\0') {
        unsigned char c = (unsigned char)mount_point[i++];
        const int needs_escape =
            !((c >= 'a' && c <= 'z') ||
              (c >= 'A' && c <= 'Z') ||
              (c >= '0' && c <= '9') ||
              c == '-' || c == '.');
        if (needs_escape) {
            if (j + 4 > out_cap) break; /* room for "_HH\0" */
            out[j++] = '_';
            out[j++] = HEX[(c >> 4) & 0xF];
            out[j++] = HEX[c & 0xF];
        } else {
            if (j + 2 > out_cap) break; /* room for "X\0" */
            out[j++] = (char)c;
        }
    }
    out[j] = '\0';
}

/* Read the .src tracker written at mount time. Returns 1 on success
 * with out filled; 0 if no tracker exists (unknown source — likely a
 * mount from before this tracking was added, or a hand-crafted one).
 * Silent failure: the Volumes screen tolerates a missing source. */
static int mount_tracker_read(const char *mount_point, char *out, size_t out_cap) {
    char key[256];
    char tracker[512];
    /* Defensive cap check: callers pass meaningfully-sized buffers
     * (256+ bytes in every current site), but require at least 2
     * bytes so we have room for a single byte read plus its NUL.
     * out_cap == 1 would mean read(fd, out, 0) — a no-op returning
     * 0 — and the buffer would be left without a terminator, which
     * a caller that bypasses the return code and reads `out`
     * directly would mishandle. Pre-zero too: the "0 bytes read
     * from a real tracker file" branch must never return
     * uninitialized stack memory. */
    if (!out || out_cap < 2) return 0;
    out[0] = '\0';
    mount_tracker_key(mount_point, key, sizeof(key));
    if (key[0] == '\0') return 0;
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, key);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return 0;
    int fd = open(tracker, O_RDONLY);
    if (fd < 0) return 0;
    ssize_t got = read(fd, out, out_cap - 1);
    close(fd);
    if (got <= 0) return 0; /* out[0] already '\0' from pre-zero above */
    out[got] = '\0';
    /* Strip trailing newline if present (hand-edited files might have one). */
    if (out[got - 1] == '\n') out[got - 1] = '\0';
    return 1;
}

/* Cheap "does a tracker file exist for this mount_point?" check.
 * Used by FS_LIST_VOLUMES to decide whether a mount belongs to us
 * (so it bypasses the writable / total>0 placeholder filters that
 * would otherwise hide our zero-free-space images). Distinct from
 * mount_tracker_read() which reads the contents — the existence
 * check costs one stat() instead of an open/read/close cycle. */
static int mount_tracker_exists(const char *mount_point) {
    char key[256];
    char tracker[512];
    mount_tracker_key(mount_point, key, sizeof(key));
    if (key[0] == '\0') return 0;
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, key);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return 0;
    struct stat st;
    return stat(tracker, &st) == 0 && S_ISREG(st.st_mode);
}

/* Write the source-path tracker for a mount. Failure is non-fatal —
 * the mount itself has already succeeded; losing the tracker just
 * means the Volumes screen won't know which file backs the mount.
 *
 * Atomic via temp + rename: a payload that crashes between
 * `open` and `close` of the destination would otherwise leave a
 * partial/empty tracker that mount_tracker_read would surface as a
 * truncated source path on next boot. The temp file lives next to
 * the destination so the rename is intra-directory (rename(2) is
 * atomic on the same filesystem). PID + thread id are appended to
 * the temp name so two parallel writes for the same mount_point —
 * across processes OR across threads in the same process — can't
 * clobber each other's temp file mid-rename. Pre-2.2.52 the suffix
 * was PID-only, which silently raced for two threads of the same
 * process (one writer's bytes truncated by the other's O_TRUNC). */
static void mount_tracker_write(const char *mount_point, const char *src_path) {
    char key[256];
    char tracker[512];
    char tracker_tmp[640];
    mount_tracker_key(mount_point, key, sizeof(key));
    if (key[0] == '\0') return;
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, key);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return;
    /* pthread_self() return type is opaque; cast through uintptr_t for
     * a stable per-thread integer suffix. The suffix only needs to
     * disambiguate concurrent writers — collisions across distinct
     * (process, thread) pairs are vanishingly unlikely on PS5. */
    n = snprintf(tracker_tmp, sizeof(tracker_tmp), "%s.tmp.%d.%lx",
                 tracker, (int)getpid(),
                 (unsigned long)(uintptr_t)pthread_self());
    if (n < 0 || (size_t)n >= sizeof(tracker_tmp)) return;
    int fd = open(tracker_tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return;
    size_t len = strlen(src_path);
    ssize_t w = (len > 0) ? write(fd, src_path, len) : 0;
    int write_err = (w < 0 || (size_t)w != len) ? errno : 0;
    /* fsync isn't required on PS5 for tracker durability — the
     * Volumes screen tolerates a missing tracker by design — but
     * close(2) flushing the page cache before the rename is needed
     * so a reader on a different fd sees the bytes. */
    close(fd);
    if (write_err != 0) {
        (void)unlink(tracker_tmp);
        return;
    }
    if (rename(tracker_tmp, tracker) != 0) {
        (void)unlink(tracker_tmp);
    }
}

/* Remove the tracker when a mount goes away. Tolerant of already-gone
 * trackers because reconciliation may have cleaned up orphans first. */
static void mount_tracker_remove(const char *mount_point) {
    char key[256];
    char tracker[512];
    mount_tracker_key(mount_point, key, sizeof(key));
    if (key[0] == '\0') return;
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, key);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return;
    (void)unlink(tracker);
}

int runtime_init(runtime_state_t *state) {
    if (!state) return -1;
    memset(state, 0, sizeof(*state));
    /* Instance ID needs to distinguish payloads loaded sub-second apart.
     * The pre-2.2.28 `time(NULL)` had second resolution: two ELFs loaded
     * within the same second produced identical IDs, so the engine's
     * "different process now" detector silently missed the takeover.
     * Mix nanoseconds + getpid() into the low 32 bits so the ID is
     * monotone-distinct even on rapid-cycle reloads.
     *
     * Layout (64 bits, MSB → LSB):
     *   bits 63..32: low 32 bits of unix-epoch seconds (`tv_sec`).
     *                Wraps in 2106; fine for ps5upload's lifetime.
     *   bits 31..16: low 16 bits of pid (Linux/FreeBSD PIDs typically
     *                fit; high-pid systems lose discrimination here
     *                but two consecutive payloads still differ via
     *                tv_nsec entropy below).
     *   bits 15..0:  low 16 bits of `tv_nsec`, XOR'd with the pid
     *                bits when the shift overlaps. Provides sub-µs
     *                resolution distinct between rapid restarts. */
    {
        struct timespec rts;
        if (clock_gettime(CLOCK_REALTIME, &rts) != 0) {
            rts.tv_sec = time(NULL);
            rts.tv_nsec = 0;
        }
        uint64_t hi = ((uint64_t)rts.tv_sec & 0xFFFFFFFFu) << 32;
        uint64_t lo = ((uint64_t)getpid() << 16) ^ ((uint64_t)rts.tv_nsec & 0xFFFFu);
        state->instance_id = hi | (lo & 0xFFFFFFFFu);
        state->started_at_unix = (uint64_t)rts.tv_sec;
    }
    state->runtime_port     = PS5UPLOAD2_RUNTIME_PORT;
    state->mgmt_port        = PS5UPLOAD2_MGMT_PORT;
    state->listener_fd      = -1;
    state->mgmt_listener_fd = -1;
    state->startup_reason   = PS5UPLOAD2_STARTUP_FRESH;
    /* Mutex guards cross-thread reads/writes of tx counters + tx_entries.
     * Single-threaded in the transfer path; only STATUS + QUERY_TX from
     * the mgmt thread need synchronization. */
    if (pthread_mutex_init(&state->state_mtx, NULL) != 0) {
        fprintf(stderr, "[payload2] pthread_mutex_init failed\n");
        return -1;
    }
    /* Per-slot mutexes for the tx table — guard each entry against
     * concurrent SHARD/COMMIT/ABORT racing on the same slot, and let
     * eviction skip slots a handler is still touching. */
    if (runtime_init_entry_mutexes() != 0) {
        fprintf(stderr, "[payload2] entry-mutex pool init failed\n");
        return -1;
    }
    snprintf(state->ownership_path, sizeof(state->ownership_path),
             "%s/active_instance.txt", PS5UPLOAD2_RUNTIME_DIR);
    snprintf(state->tx_state_path, sizeof(state->tx_state_path),
             "%s/runtime_tx_state.txt", PS5UPLOAD2_TX_DIR);
    snprintf(state->tx_journal_path, sizeof(state->tx_journal_path),
             "%s/events.log", PS5UPLOAD2_TX_DIR);
    if (runtime_load_tx_state(state) != 0) {
        fprintf(stderr, "[payload2] failed to load tx state from %s\n", state->tx_state_path);
        return -1;
    }
    backup_init();
    remoteplay_init();
    notif_init();
    if (runtime_load_tx_entries(state) != 0) {
        fprintf(stderr, "[payload2] failed to load tx entries from %s\n", PS5UPLOAD2_TX_DIR);
        return -1;
    }
    runtime_reconcile_tx_state(state);
    state->recovered_transactions = state->active_transactions;
    if (state->recovered_transactions > 0) {
        (void)runtime_append_tx_event(state, "startup_replay");
    }
    return 0;
}

int runtime_write_ownership(const runtime_state_t *state) {
    FILE *fp = NULL;
    int fd_for_sync;
    char tmp_path[300];
    if (!state) return -1;
    /* Atomic write via tmp + rename. Pre-2.2.28 used `fopen("w")`
     * which truncates the destination immediately — a second payload
     * starting during this call could observe an empty ownership
     * file, and a power loss between truncate and fclose would leave
     * a permanently-empty record. Rename(2) on POSIX is atomic, so a
     * concurrent reader either sees the old file or the new one,
     * never half-written. */
    int n = snprintf(tmp_path, sizeof(tmp_path), "%s.tmp",
                     state->ownership_path);
    if (n < 0 || (size_t)n >= sizeof(tmp_path)) {
        fprintf(stderr, "[payload2] ownership tmp path overflow for %s\n",
                state->ownership_path);
        return -1;
    }
    fp = fopen(tmp_path, "w");
    if (!fp) {
        fprintf(stderr, "[payload2] failed to open ownership tmp %s\n", tmp_path);
        return -1;
    }
    fprintf(fp,
            "instance_id=%llu\nruntime_port=%d\nstartup_reason=%d\nstarted_at_unix=%llu\npid=%d\n",
            (unsigned long long)state->instance_id,
            state->runtime_port,
            state->startup_reason,
            (unsigned long long)state->started_at_unix,
            (int)getpid());
    /* Flush + fsync before rename so the bytes are durable. Without
     * fsync the rename can promote stale (or zero) content into the
     * destination if a crash hits before writeback. */
    if (fflush(fp) != 0) {
        fclose(fp);
        (void)unlink(tmp_path);
        fprintf(stderr, "[payload2] fflush ownership tmp %s failed\n", tmp_path);
        return -1;
    }
    fd_for_sync = fileno(fp);
    if (fd_for_sync >= 0) (void)fsync(fd_for_sync);
    if (fclose(fp) != 0) {
        (void)unlink(tmp_path);
        fprintf(stderr, "[payload2] fclose ownership tmp %s failed\n", tmp_path);
        return -1;
    }
    if (rename(tmp_path, state->ownership_path) != 0) {
        fprintf(stderr, "[payload2] rename %s -> %s failed: %s\n",
                tmp_path, state->ownership_path, strerror(errno));
        (void)unlink(tmp_path);
        return -1;
    }
    printf("[payload2] ownership record instance=%llu port=%d path=%s\n",
           (unsigned long long)state->instance_id,
           state->runtime_port,
           state->ownership_path);
    return 0;
}

int runtime_clear_ownership(const runtime_state_t *state) {
    if (!state) return -1;
    if (unlink(state->ownership_path) == 0 || errno == ENOENT) return 0;
    fprintf(stderr, "[payload2] failed to remove ownership record %s\n",
            state->ownership_path);
    return -1;
}

/* Read the `pid=` line out of the PREVIOUS ownership record (before we
 * overwrite it). Returns the pid, or -1 if absent/unreadable. */
static int runtime_read_prior_pid(const char *ownership_path) {
    FILE *fp = fopen(ownership_path, "r");
    if (!fp) return -1;
    int pid = -1;
    char line[128];
    while (fgets(line, sizeof(line), fp)) {
        if (sscanf(line, "pid=%d", &pid) == 1) break;
    }
    fclose(fp);
    return pid;
}

/* Read the `started_at_unix` line from the prior ownership record. Returns
 * the recorded wall-clock start time (CLOCK_REALTIME seconds), or 0 if the
 * field is absent (older-format record) or unreadable. */
static uint64_t runtime_read_prior_started_at(const char *ownership_path) {
    FILE *fp = fopen(ownership_path, "r");
    if (!fp) return 0;
    unsigned long long started = 0;
    char line[128];
    while (fgets(line, sizeof(line), fp)) {
        if (sscanf(line, "started_at_unix=%llu", &started) == 1) break;
    }
    fclose(fp);
    return (uint64_t)started;
}

/* System boot wall-clock time (CLOCK_REALTIME seconds) via sysctl
 * kern.boottime. Same clock domain as `started_at_unix`, so the two are
 * directly comparable to tell whether an ownership record was written in
 * the CURRENT boot session. Returns 0 if the sysctl is unavailable — callers
 * must treat 0 as "unknown" and refuse to act on it. */
static uint64_t runtime_system_boottime_unix(void) {
    struct timeval bt;
    size_t len = sizeof(bt);
    int mib[2] = {CTL_KERN, KERN_BOOTTIME};
    if (sysctl(mib, 2, &bt, &len, NULL, 0) != 0 || len < sizeof(bt)) return 0;
    if (bt.tv_sec <= 0) return 0;
    return (uint64_t)bt.tv_sec;
}

/*
 * Best-effort reap of a previous payload instance that crashed and lingered.
 *
 * The cooperative takeover (runtime_try_takeover) only makes a HEALTHY old
 * instance exit — it sends TAKEOVER_REQUEST and waits for the ports to free.
 * A *crashed* instance can't honor that request (its listener thread is dead,
 * or a worker is wedged), so it's left running. Every resend then starts a new
 * instance alongside it → the "duplicate payload.elf" the user sees.
 *
 * Here we read the pid the previous instance recorded in the ownership file
 * and SIGKILL it outright. Safety: we only kill a pid that (a) we recorded
 * ourselves, (b) is alive, (c) isn't us, and (d) still has the SAME process
 * name we do — so a recycled pid now owned by an unrelated (e.g. system)
 * process is never touched. We deliberately do NOT kill "every process named
 * like us": some loaders name all payloads generically (e.g. "payload.elf"),
 * and a name-sweep could take down kstuff/SMP/etc.
 *
 * A process wedged in uninterruptible kernel sleep won't die even from
 * SIGKILL — only a PS5 reboot clears those. We log that case clearly (to
 * stderr, which the bug-report bundle captures) so the user can be told.
 *
 * Must be called AFTER runtime_try_takeover and BEFORE runtime_write_ownership
 * (which overwrites the prior record with our own pid).
 */
void runtime_reap_prior_instance(runtime_state_t *state) {
    if (!state) return;
    int prior = runtime_read_prior_pid(state->ownership_path);
    int me = (int)getpid();
    /* Diagnostic (also exercises proc_name_by_pid on every startup so the
     * kinfo offsets are validated on this firmware even when there's nothing
     * to reap). Goes to stderr → stderr.log → bug bundle. */
    {
        char self_name[64] = {0};
        int ok = proc_name_by_pid(me, self_name, sizeof(self_name));
        fprintf(stderr, "[payload2] reap check: pid=%d self_name=%s prior_pid=%d\n",
                me, ok == 0 ? self_name : "<unknown>", prior);
    }
    if (prior <= 0 || prior == me) return;

    /* Boot-session guard (the critical safety gate). The ownership file lives
     * on persistent /data and SURVIVES reboots. After a reboot the kernel's
     * PID counter resets, so a stale `pid=` from a PRIOR boot is very likely
     * now owned by UNRELATED homebrew the user autoloaded (a cheat loader,
     * nanoDNS, ...). SIGKILLing it would take down their other tools — exactly
     * the "my scripts died" reports. So only reap a record written during the
     * CURRENT boot: the recorded start time must be >= this boot's wall-clock
     * start. If either value is unknown (no sysctl, or an old-format record
     * with no started_at), refuse to kill — the worst case is the pre-existing
     * "restart the PS5" fallback, which is far better than killing a
     * bystander. The name check below is kept as a second line of defense. */
    {
        uint64_t prior_started =
            runtime_read_prior_started_at(state->ownership_path);
        uint64_t boottime = runtime_system_boottime_unix();
        if (boottime == 0 || prior_started == 0 || prior_started < boottime) {
            fprintf(stderr,
                    "[payload2] reap: prior record (started=%llu, boottime=%llu) "
                    "is from a previous boot or unverifiable — pid %d may now be "
                    "unrelated homebrew; NOT killing\n",
                    (unsigned long long)prior_started,
                    (unsigned long long)boottime, prior);
            return;
        }
    }

    if (kill((pid_t)prior, 0) != 0) return; /* already gone */

    char my_name[64] = {0};
    char their_name[64] = {0};
    if (proc_name_by_pid(me, my_name, sizeof(my_name)) != 0) {
        /* Can't establish our own name → can't safely compare → don't kill. */
        fprintf(stderr, "[payload2] reap: cannot read own process name — skipping\n");
        return;
    }
    if (proc_name_by_pid(prior, their_name, sizeof(their_name)) != 0) {
        return; /* prior pid vanished between the checks — nothing to do */
    }
    if (strcmp(my_name, their_name) != 0) {
        fprintf(stderr,
                "[payload2] reap: pid %d is '%s', not our '%s' — recycled pid, skipping\n",
                prior, their_name, my_name);
        return;
    }

    fprintf(stderr, "[payload2] reaping crashed prior instance pid=%d (name=%s)\n",
            prior, their_name);
    kill((pid_t)prior, SIGKILL);

    /* Confirm it actually died (poll ~1 s). A survivor is kernel-wedged. */
    for (int i = 0; i < 20; i++) {
        if (kill((pid_t)prior, 0) != 0) {
            fprintf(stderr, "[payload2] reaped prior instance pid=%d\n", prior);
            return;
        }
        usleep(50000);
    }
    fprintf(stderr,
            "[payload2] prior instance pid=%d survived SIGKILL (kernel-wedged) — "
            "a PS5 reboot is required to clear it\n",
            prior);
}

/* ── Shutdown watchdog ────────────────────────────────────────────────────── */

static int g_watchdog_exit_code = 0;

static void *runtime_shutdown_watchdog(void *arg) {
    (void)arg;
    /* Grace period for the normal close+join below. If shutdown is healthy the
     * process exits via main()'s return long before this fires and this thread
     * dies with it. If shutdown WEDGES (e.g. pthread_join on a mgmt thread
     * stuck in an uninterruptible Sony API never returns), force the process
     * out so it can't linger as an orphan the next resend would duplicate. */
    sleep(8);
    fprintf(stderr, "[payload2] shutdown watchdog fired — forcing _exit(%d)\n",
            g_watchdog_exit_code);
    _exit(g_watchdog_exit_code);
    return NULL;
}

void runtime_arm_shutdown_watchdog(int exit_code) {
    g_watchdog_exit_code = exit_code;
    pthread_t t;
    if (pthread_create(&t, NULL, runtime_shutdown_watchdog, NULL) == 0) {
        pthread_detach(t);
    }
}

/* ── Wire I/O ─────────────────────────────────────────────────────────────────── */

static int recv_exact(int fd, void *buf, size_t len) {
    unsigned char *p = (unsigned char *)buf;
    size_t got = 0;
    while (got < len) {
        ssize_t n = recv(fd, p + got, len - got, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            /* Log the actual cause so a stalled transfer produces a
             * diagnostic in the engine/renderer log, not silence.
             * EAGAIN / EWOULDBLOCK here means SO_RCVTIMEO fired — the
             * client hasn't sent data within the idle window. Keep it
             * distinguishable from other recv errors. */
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                fprintf(stderr,
                        "[payload] recv_exact(fd=%d): timed out after %zu/%zu bytes "
                        "(SO_RCVTIMEO expired, client stalled)\n",
                        fd, got, len);
            } else {
                fprintf(stderr,
                        "[payload] recv_exact(fd=%d): got %zu/%zu bytes, recv failed: %s (errno=%d)\n",
                        fd, got, len, strerror(errno), errno);
            }
            return -1;
        }
        if (n == 0) {
            /* Peer closed the connection mid-frame. Usually means the
             * client crashed or the user cancelled — not a bug on our
             * side, but logging the byte-count we reached helps
             * distinguish "nothing arrived" from "truncated after K
             * bytes". */
            fprintf(stderr,
                    "[payload] recv_exact(fd=%d): peer closed after %zu/%zu bytes\n",
                    fd, got, len);
            return -1;
        }
        got += (size_t)n;
    }
    return 0;
}

/* Monotonic microseconds for per-phase timing. */
static uint64_t now_us(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (uint64_t)ts.tv_sec * 1000000ull + (uint64_t)(ts.tv_nsec / 1000);
}

static uint16_t read_le16(const unsigned char *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t read_le32(const unsigned char *p) {
    return (uint32_t)p[0]
         | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

static uint64_t read_le64(const unsigned char *p) {
    uint64_t v = 0;
    int i = 0;
    for (i = 0; i < 8; i++) v |= ((uint64_t)p[i]) << (8 * i);
    return v;
}

static void write_le16(unsigned char *p, uint16_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >> 8) & 0xff);
}

static void write_le32(unsigned char *p, uint32_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >> 8) & 0xff);
    p[2] = (unsigned char)((v >> 16) & 0xff);
    p[3] = (unsigned char)((v >> 24) & 0xff);
}

static void write_le64(unsigned char *p, uint64_t v) {
    int i = 0;
    for (i = 0; i < 8; i++) p[i] = (unsigned char)((v >> (8 * i)) & 0xff);
}

/* Loop until `len` bytes have been sent. Treats EINTR as retry; returns
 * 0 on full send, -1 on hard error or peer disconnect. Without this loop,
 * a single short write (common when SO_SNDBUF fills under load) leaves
 * the framing desynced and the next dispatcher iteration reads garbage
 * as a header — manifests to users as random "bad_magic" disconnects. */
static int send_full(int fd, const void *buf, size_t len) {
    const unsigned char *p = (const unsigned char *)buf;
    size_t remaining = len;
    while (remaining > 0) {
        ssize_t n = send(fd, p, remaining, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        p += (size_t)n;
        remaining -= (size_t)n;
    }
    return 0;
}

static int send_frame(int fd, uint16_t frame_type, uint32_t flags,
                      uint64_t trace_id, const void *body, uint64_t body_len) {
    unsigned char hdr[FTX2_HEADER_LEN];
    write_le32(hdr + 0, FTX2_MAGIC);
    write_le16(hdr + 4, FTX2_VERSION);
    write_le16(hdr + 6, frame_type);
    write_le32(hdr + 8, flags);
    write_le64(hdr + 12, body_len);
    write_le64(hdr + 20, trace_id);
    if (send_full(fd, hdr, sizeof(hdr)) != 0) return -1;
    if (body_len > 0 && body) {
        if (send_full(fd, body, (size_t)body_len) != 0) return -1;
    }
    return 0;
}

/* ── TX meta parsing ──────────────────────────────────────────────────────────── */

static int parse_tx_meta(const char *body, uint64_t body_len,
                          ftx2_tx_meta_t *out,
                          const char **extra, uint64_t *extra_len) {
    if (!body || !out) return -1;
    memset(out, 0, sizeof(*out));
    if (body_len < FTX2_TX_META_LEN) return -1;
    memcpy(out->tx_id, body, 16);
    out->kind  = read_le32((const unsigned char *)body + 16);
    out->flags = read_le32((const unsigned char *)body + 20);
    if (extra)     *extra     = body + FTX2_TX_META_LEN;
    if (extra_len) *extra_len = body_len - FTX2_TX_META_LEN;
    return 0;
}

/* ── STREAM_SHARD handler ─────────────────────────────────────────────────────── */

/* Drain exactly `remaining` bytes from `fd` in 64 KiB chunks. */
static int drain_shard_data(int fd, uint64_t remaining) {
    char buf[FTX2_SHARD_DRAIN_BUF];
    while (remaining > 0) {
        size_t take = remaining > sizeof(buf) ? sizeof(buf) : (size_t)remaining;
        if (recv_exact(fd, buf, take) != 0) return -1;
        remaining -= (uint64_t)take;
    }
    return 0;
}

/*
 * Write shard data from the socket into a spool file.
 * Falls back to drain if the spool directory cannot be created or the file
 * cannot be opened (so the connection is never left in a bad read state).
 *
 * Spool layout:  PS5UPLOAD2_SPOOL_DIR/spool_<tx_id_hex>/<shard_seq>
 */
/*
 * Write `len` bytes from `buf` to `fd` with retry on short writes / EINTR.
 * Returns 0 on success, -1 on any permanent I/O error.
 */
static int write_full(int fd, const void *buf, size_t len) {
    const unsigned char *p = (const unsigned char *)buf;
    size_t written = 0;
    while (written < len) {
        ssize_t w = write(fd, p + written, len - written);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (w == 0) return -1;
        written += (size_t)w;
    }
    return 0;
}

/* ── Double-buffered writer thread ────────────────────────────────────────────
 *
 * The shard receive loop is recv-then-write per chunk, serialised on the main
 * thread. For a 128 MiB transfer on PS5, recv takes ~2.3 s and write takes
 * ~4 s — overlapping them with a helper thread lets the two phases run in
 * parallel, bounded by `max(recv, write)` instead of the sum.
 *
 * Design: exactly two buffers. The producer (main thread) fills one while the
 * consumer (writer thread) drains the other, then they swap. One mutex, one
 * condvar per slot. Consumer exits on a sentinel `len < 0`.
 */

typedef struct {
    unsigned char *buf;
    ssize_t        len;  /* 0 = empty (writer may refill), >0 = full (writer must drain), <0 = shutdown */
} piped_slot_t;

typedef struct {
    piped_slot_t    slot[2];
    pthread_mutex_t lock;
    pthread_cond_t  cv_full[2];   /* signaled when slot[i] becomes full */
    pthread_cond_t  cv_empty[2];  /* signaled when slot[i] becomes empty */
    int             fd;
    int             writer_error; /* set by writer on I/O failure */
} piped_writer_t;

/* Create a JOINABLE worker thread with a bounded stack. The pack workers,
 * piped writers, and async-copy writer run trivial recv→write/copy loops
 * (their only stack locals are a 1 KiB scratch path at most; all real buffers
 * are heap). The FreeBSD default thread stack is multiple MiB, and under 4
 * concurrent packed streams there can be 16+ of these threads at once — the
 * accumulated default-stack reservation needlessly tightens the small PS5
 * per-process memory budget (a plausible OOM contributor at 4 streams). 512
 * KiB matches the mgmt accept-loop thread (proven stable across firmwares) and
 * leaves a large margin over any worker call chain. Falls back to the default
 * stack if attr setup fails, so behavior never regresses. The detach state is
 * left at the default (joinable) — every caller joins these threads. */
static int create_worker_thread(pthread_t *tid, void *(*fn)(void *), void *arg) {
    pthread_attr_t attr;
    pthread_attr_t *attr_p = NULL;
    if (pthread_attr_init(&attr) == 0) {
        (void)pthread_attr_setstacksize(&attr, 512u * 1024u);
        attr_p = &attr;
    }
    int rc = pthread_create(tid, attr_p, fn, arg);
    if (attr_p) pthread_attr_destroy(attr_p);
    return rc;
}

/*
 * Persistent writer handle attached to runtime_tx_entry_t::direct_writer.
 *
 * Lifecycle (single-file direct mode, total_bytes >= threshold):
 *   BEGIN_TX       → nothing (lazy)
 *   first shard    → direct_writer_start() mallocs this struct + 2× io bufs,
 *                    pthread_creates the writer thread, stores the handle
 *   each shard     → producer reuses pw->slot[direct_slot] just like today's
 *                    inline writer path, but never sends the sentinel
 *   COMMIT_TX      → direct_writer_finish() sends sentinel + joins + returns
 *                    pw->writer_error
 *   any terminal   → direct_writer_discard() sends sentinel, joins, frees,
 *                    ignores writer_error (we're unlinking the tmp anyway)
 *
 * The writer thread itself is vanilla piped_writer_thread() — no change.
 * All state the thread touches lives in `pw` (heap), so tx_entry re-shuffling
 * or slot eviction during background writes is safe as long as we haven't
 * freed `pw` yet. direct_writer_discard() is the only legal free path. */
typedef struct {
    piped_writer_t pw;          /* first so &handle == &pw (mutex init unchanged) */
    pthread_t      tid;
    int            started;     /* 1 when tid is joinable */
    unsigned char *bufs[2];     /* heap-owned double buffers (also in pw.slot[i].buf) */
} direct_writer_handle_t;

/* Forward declarations for the persistent direct-write helpers. */
static int  direct_writer_start(runtime_tx_entry_t *entry, int fd);
static int  direct_writer_finish(runtime_tx_entry_t *entry);
static void direct_writer_discard(runtime_tx_entry_t *entry);

static void *piped_writer_thread(void *arg) {
    piped_writer_t *pw = (piped_writer_t *)arg;
    int i = 0;
    /* Bytes written since the last periodic fsync. Bounds the kernel
     * dirty-page backlog for huge single-file streams so the kernel's
     * dirty-page write-throttle never engages and collapses throughput. See
     * PS5UPLOAD2_WRITEBACK_FSYNC_BYTES. */
    uint64_t since_fsync = 0;
    for (;;) {
        ssize_t len;
        unsigned char *buf;
        pthread_mutex_lock(&pw->lock);
        while (pw->slot[i].len == 0) {
            pthread_cond_wait(&pw->cv_full[i], &pw->lock);
        }
        len = pw->slot[i].len;
        buf = pw->slot[i].buf;
        pthread_mutex_unlock(&pw->lock);

        if (len < 0) break; /* shutdown */
        if (write_full(pw->fd, buf, (size_t)len) != 0) {
            pthread_mutex_lock(&pw->lock);
            pw->writer_error = 1;
            /* Mark BOTH slots empty so a producer waiting on the
             * other slot's cv_empty wakes up too. The prior code
             * only signaled cv_empty[i], which left a producer
             * blocked on cv_empty[1-i] stuck until the recv timeout
             * eventually broke the connection — observably as
             * "transfer hangs forever" after a write_full failure. */
            pw->slot[0].len = 0;
            pw->slot[1].len = 0;
            pthread_cond_broadcast(&pw->cv_empty[0]);
            pthread_cond_broadcast(&pw->cv_empty[1]);
            pthread_mutex_unlock(&pw->lock);
            return NULL;
        }
        /* Periodically flush so the kernel's dirty-page backlog can't grow
         * unbounded on a huge single-file stream and throttle write(2) to a
         * crawl. fsync converts the dirty pages to clean (reclaimable) ones,
         * so the dirty count stays bounded and throughput settles at the
         * storage's sustained rate instead of collapsing late in the transfer.
         * Done OUTSIDE the lock (between write and the empty-signal) so it adds
         * natural backpressure — the producer blocks on a full slot during the
         * flush, which is exactly the pacing we want. Disabled when the
         * interval is 0. A flush failure isn't fatal here: COMMIT_TX still does
         * a final fsync, and a real I/O error surfaces via write_full above. */
#if PS5UPLOAD2_WRITEBACK_FSYNC_BYTES > 0
        if (len > 0) {
            since_fsync += (uint64_t)len;
            if (since_fsync >= PS5UPLOAD2_WRITEBACK_FSYNC_BYTES) {
                (void)fsync(pw->fd);
                since_fsync = 0;
            }
        }
#endif
        pthread_mutex_lock(&pw->lock);
        pw->slot[i].len = 0;
        pthread_cond_signal(&pw->cv_empty[i]);
        pthread_mutex_unlock(&pw->lock);
        i = 1 - i;
    }
    return NULL;
}

/*
 * Spawn a persistent writer thread bound to `fd` and attach it to `entry`.
 * `fd` ownership transfers to the handle on success (closed by finish/discard).
 * On failure, `fd` is left open for the caller to deal with.
 * Returns 0 on success, -1 on allocation/pthread failure.
 */
static int direct_writer_start(runtime_tx_entry_t *entry, int fd) {
    direct_writer_handle_t *h = NULL;
    if (!entry || entry->direct_writer || fd < 0) return -1;
    h = (direct_writer_handle_t *)calloc(1, sizeof(*h));
    if (!h) return -1;
    h->bufs[0] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    h->bufs[1] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    if (!h->bufs[0] || !h->bufs[1]) {
        free(h->bufs[0]);
        free(h->bufs[1]);
        free(h);
        return -1;
    }
    h->pw.fd = fd;
    h->pw.slot[0].buf = h->bufs[0];
    h->pw.slot[1].buf = h->bufs[1];
    pthread_mutex_init(&h->pw.lock, NULL);
    pthread_cond_init(&h->pw.cv_full[0], NULL);
    pthread_cond_init(&h->pw.cv_full[1], NULL);
    pthread_cond_init(&h->pw.cv_empty[0], NULL);
    pthread_cond_init(&h->pw.cv_empty[1], NULL);
    if (create_worker_thread(&h->tid, piped_writer_thread, &h->pw) != 0) {
        pthread_mutex_destroy(&h->pw.lock);
        pthread_cond_destroy(&h->pw.cv_full[0]);
        pthread_cond_destroy(&h->pw.cv_full[1]);
        pthread_cond_destroy(&h->pw.cv_empty[0]);
        pthread_cond_destroy(&h->pw.cv_empty[1]);
        free(h->bufs[0]);
        free(h->bufs[1]);
        free(h);
        return -1;
    }
    h->started = 1;
    entry->direct_writer = h;
    entry->direct_slot   = 0;
    return 0;
}

/*
 * Drain the persistent writer cleanly on the success path: send the `len<0`
 * sentinel on the next producer slot, wait for it to be accepted, join the
 * thread, tear down sync primitives + buffers + fd, clear the handle.
 * Returns 0 if the writer reported no I/O error, -1 otherwise.
 *
 * `direct_fd_open` / `direct_fd` on the entry are cleared by this function
 * because the handle owns the fd once direct_writer_start succeeds.
 */
static int direct_writer_finish(runtime_tx_entry_t *entry) {
    direct_writer_handle_t *h = NULL;
    int writer_error = 0;
    int slot = 0;
    if (!entry || !entry->direct_writer) return 0;
    h = (direct_writer_handle_t *)entry->direct_writer;
    slot = entry->direct_slot;
    if (slot < 0 || slot > 1) slot = 0;
    /* Wait for the sentinel slot to be free, then post the shutdown marker. */
    pthread_mutex_lock(&h->pw.lock);
    while (h->pw.slot[slot].len > 0 && !h->pw.writer_error) {
        pthread_cond_wait(&h->pw.cv_empty[slot], &h->pw.lock);
    }
    h->pw.slot[slot].len = -1;
    pthread_cond_signal(&h->pw.cv_full[slot]);
    pthread_mutex_unlock(&h->pw.lock);
    if (h->started) pthread_join(h->tid, NULL);
    writer_error = h->pw.writer_error;
    pthread_mutex_destroy(&h->pw.lock);
    pthread_cond_destroy(&h->pw.cv_full[0]);
    pthread_cond_destroy(&h->pw.cv_full[1]);
    pthread_cond_destroy(&h->pw.cv_empty[0]);
    pthread_cond_destroy(&h->pw.cv_empty[1]);
    free(h->bufs[0]);
    free(h->bufs[1]);
    if (entry->direct_fd_open) {
        close(entry->direct_fd);
        entry->direct_fd_open = 0;
        entry->direct_fd      = -1;
    }
    free(h);
    entry->direct_writer = NULL;
    entry->direct_slot   = 0;
    return writer_error ? -1 : 0;
}

/*
 * Teardown variant for abort/takeover/shutdown/fatal paths. Sends the
 * sentinel + joins + frees exactly like finish(), but ignores writer_error
 * (the caller is about to unlink the tmp anyway). Idempotent and safe to
 * call when the writer was never started. Also closes direct_fd if it was
 * opened without spawning a writer (small-tx path below the threshold).
 */
static void direct_writer_discard(runtime_tx_entry_t *entry) {
    if (!entry) return;
    if (entry->direct_writer) {
        (void)direct_writer_finish(entry);
    } else if (entry->direct_fd_open) {
        close(entry->direct_fd);
        entry->direct_fd_open = 0;
        entry->direct_fd      = -1;
    }
}

/* ── Packed-shard worker pool ─────────────────────────────────────────────────
 *
 * For many-small-file workloads, per-record `open + write + close` dominates.
 * Serialising it in the main thread caps throughput at the FS's per-file cost
 * (~23 ms/file on PS5 UFS), wasting the wire (NIC is ~100× faster than disk
 * metadata). A small pool of kernel threads lets metadata operations overlap
 * across files — the real gain is "files in different directories can be
 * created concurrently"; a secondary gain is "open/write/close of one file
 * overlap with open/write/close of another in the same dir".
 *
 * Lifecycle mirrors the single-file persistent writer (runtime_release_tx_resources
 * is the sole teardown site):
 *   first packed shard   → pack_pool_start() mallocs handle + spawns workers
 *   every record         → reader pushes (path, data); worker pops + writes
 *   end of each shard    → pack_pool_drain() waits for queue empty before ACK,
 *                          preserving the "SPOOLED == persisted" semantic the
 *                          protocol promises on StreamShard ACK
 *   terminal transition  → pack_pool_teardown() drains + shutdown + join,
 *                          sums per-worker timings into the tx entry so they
 *                          surface in COMMIT_TX_ACK, frees all memory
 *
 * Safety notes:
 *   - reader owns the fs-metadata cache (last_parent_dir); workers don't
 *     touch it, so no mutex needed for the mkdir-p cache
 *   - workers drain the queue even after worker_error is set, so the reader
 *     is never blocked on a permanently-stuck pool_drain
 *   - worker errors are sticky; no retry semantics inside the pool
 */

typedef struct pack_work_item {
    char          *path;      /* NUL-terminated, malloc'd by reader, freed by worker */
    unsigned char *data;      /* may be NULL when data_len == 0 */
    uint32_t       data_len;
} pack_work_item_t;

typedef struct pack_worker_pool {
    pthread_t        tid[PS5UPLOAD2_PACK_WORKERS];
    int              tid_started;      /* count of actually-created threads */
    pack_work_item_t queue[PS5UPLOAD2_PACK_QUEUE_DEPTH];
    int              q_head;           /* next slot to pop */
    int              q_tail;           /* next slot to push */
    int              q_count;
    int              shutdown;         /* set on teardown; workers drain then exit */
    int              worker_error;     /* sticky I/O failure flag */
    pthread_mutex_t  lock;
    pthread_cond_t   cv_not_full;      /* reader waits when queue full */
    pthread_cond_t   cv_not_empty;     /* workers wait when queue empty */
    pthread_cond_t   cv_drained;       /* reader waits during drain */
    /* Accumulators — updated by workers under `lock` at end of each item.
     * Contention is minimal (4 workers, each update is a few arithmetic ops
     * and a conditional signal), so sharing one mutex keeps code simple. */
    uint64_t         t_unlink_us;
    uint64_t         t_open_us;
    uint64_t         t_ftruncate_us;
    uint64_t         t_write_us;
    uint64_t         t_close_us;
    uint64_t         records_processed;
    uint64_t         open_retries;     /* transient open() retries absorbed */
    uint64_t         write_retries;    /* transient write_full() retries absorbed */
} pack_worker_pool_t;

/* Errnos we treat as worth retrying on the pack-worker hot path. EIO covers
 * USB media hiccups; EMFILE/ENFILE cover fd-table pressure under sustained
 * many-small-file load; ENOMEM/EBUSY are transient resource hits; EAGAIN
 * shouldn't normally hit a blocking open/write but is cheap to allow. */
static int pack_errno_is_transient(int e) {
    switch (e) {
        case EINTR:
        case EAGAIN:
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
        case EWOULDBLOCK:
#endif
        case EIO:
        case EMFILE:
        case ENFILE:
        case ENOMEM:
        case EBUSY:
            return 1;
        default:
            return 0;
    }
}

static void pack_retry_backoff(int retry_idx) {
    static const useconds_t backoff_us[] = { 20000u, 50000u, 100000u };
    int n = (int)(sizeof(backoff_us) / sizeof(backoff_us[0]));
    if (retry_idx < 0) retry_idx = 0;
    if (retry_idx >= n) retry_idx = n - 1;
    usleep(backoff_us[retry_idx]);
}

static void *pack_worker_thread(void *arg) {
    pack_worker_pool_t *pool = (pack_worker_pool_t *)arg;
    for (;;) {
        pack_work_item_t item;
        pthread_mutex_lock(&pool->lock);
        while (pool->q_count == 0 && !pool->shutdown) {
            pthread_cond_wait(&pool->cv_not_empty, &pool->lock);
        }
        if (pool->q_count == 0 && pool->shutdown) {
            pthread_mutex_unlock(&pool->lock);
            return NULL;
        }
        item = pool->queue[pool->q_head];
        pool->q_head = (pool->q_head + 1) % PS5UPLOAD2_PACK_QUEUE_DEPTH;
        pool->q_count -= 1;
        pthread_cond_signal(&pool->cv_not_full);
        if (pool->q_count == 0) pthread_cond_signal(&pool->cv_drained);
        pthread_mutex_unlock(&pool->lock);

        if (!pool->worker_error) {
            /* Packed records write directly to their final path — no tmp +
             * rename dance. The tmp pattern's only benefit is mid-transfer
             * atomicity, but on PS5 UFS each `.ps5up2-tmp` unlink/open/rename
             * costs ~40 ms of metadata work, and the COMMIT-side rename of
             * 5000 files added another ~5 s of apply time. By writing
             * directly we lose only the "partial-file-on-abort" guarantee,
             * which is already an acceptable state: aborted multi-file
             * transfers were never cleaned up field-by-field anyway, and the
             * CLEANUP frame exists precisely to handle residual state. Huge
             * single-file direct writes still use the tmp+rename pattern
             * (`entry->tmp_path`) because there the rename is O(1) and the
             * atomicity guarantee is more valuable. */
            int fd = -1;
            uint64_t t0, t_op = 0, t_tr = 0, t_wr = 0, t_cl = 0;
            int attempt;
            uint64_t local_open_retries = 0, local_write_retries = 0;
            int terminal = 0;

            /* Bounded-retry open. A single transient open failure on USB or
             * UFS at file 11k of 75k used to abort the whole transaction;
             * absorbing it here keeps the small-file-heavy regime alive. */
            for (attempt = 0; attempt <= (int)PS5UPLOAD2_PACK_RETRY_MAX; attempt++) {
                t0 = now_us();
                fd = open(item.path, O_WRONLY | O_CREAT | O_TRUNC, 0777);
                if (fd >= 0) (void)fchmod(fd, 0777);
                t_op += now_us() - t0;
                if (fd >= 0) break;
                if (!pack_errno_is_transient(errno) ||
                    attempt == (int)PS5UPLOAD2_PACK_RETRY_MAX) {
                    fprintf(stderr,
                            "[payload2] pack worker: open %s failed errno=%d after %d retries\n",
                            item.path, errno, attempt);
                    pool->worker_error = 1;
                    terminal = 1;
                    break;
                }
                fprintf(stderr,
                        "[payload2] pack worker: open %s transient errno=%d, retry %d/%u\n",
                        item.path, errno, attempt + 1,
                        (unsigned)PS5UPLOAD2_PACK_RETRY_MAX);
                local_open_retries += 1;
                pack_retry_backoff(attempt);
            }

            if (!terminal && fd >= 0) {
                /* Historical note: an earlier experiment called
                 * `posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)` here to try
                 * to keep the page cache clean between transactions. On the
                 * e1000 lab NIC it produced a measurable throughput regression
                 * on the 1 GiB single-file path (88.65 -> 84.87 MiB/s), likely
                 * because DONTNEED forces synchronous writeback of dirty pages
                 * through the one-remaining-writer-thread bottleneck. The hint
                 * turned out to have negative ROI on this hardware, so we
                 * don't call it. Revisit when we have different NIC / workload
                 * evidence that shows it actually helping. */
                if (item.data_len > 0) {
                    t0 = now_us();
                    (void)ftruncate(fd, (off_t)item.data_len);
                    t_tr = now_us() - t0;

                    /* Bounded-retry write. write_full handles EINTR
                     * internally, so any -1 here is a non-EINTR error.
                     * On retry we close+reopen with O_TRUNC so the rewrite
                     * starts from offset 0 and produces a valid file. */
                    for (attempt = 0; attempt <= (int)PS5UPLOAD2_PACK_RETRY_MAX; attempt++) {
                        t0 = now_us();
                        if (write_full(fd, item.data, item.data_len) == 0) {
                            t_wr += now_us() - t0;
                            break;
                        }
                        t_wr += now_us() - t0;
                        if (!pack_errno_is_transient(errno) ||
                            attempt == (int)PS5UPLOAD2_PACK_RETRY_MAX) {
                            fprintf(stderr,
                                    "[payload2] pack worker: write %s failed errno=%d after %d retries\n",
                                    item.path, errno, attempt);
                            pool->worker_error = 1;
                            break;
                        }
                        fprintf(stderr,
                                "[payload2] pack worker: write %s transient errno=%d, retry %d/%u\n",
                                item.path, errno, attempt + 1,
                                (unsigned)PS5UPLOAD2_PACK_RETRY_MAX);
                        local_write_retries += 1;
                        pack_retry_backoff(attempt);

                        /* Reset the file: close, reopen with O_TRUNC,
                         * re-pre-allocate. If reopen itself fails, give up. */
                        (void)close(fd);
                        t0 = now_us();
                        fd = open(item.path, O_WRONLY | O_CREAT | O_TRUNC, 0777);
                        if (fd >= 0) (void)fchmod(fd, 0777);
                        t_op += now_us() - t0;
                        if (fd < 0) {
                            fprintf(stderr,
                                    "[payload2] pack worker: re-open %s for write retry failed errno=%d\n",
                                    item.path, errno);
                            pool->worker_error = 1;
                            break;
                        }
                        (void)ftruncate(fd, (off_t)item.data_len);
                    }
                }
                if (fd >= 0) {
                    t0 = now_us();
                    (void)close(fd);
                    t_cl = now_us() - t0;
                }
            }

            /* Pack records write directly to `<file>` and never produce a
             * `.ps5up2-tmp`. But a *stale* tmp can still exist from a
             * prior aborted run where this file was non-packed — and the
             * 2.2.29 BEGIN_TX sweep skips the resume path. If a stale
             * tmp survives to COMMIT, the rename loop promotes it over
             * our just-written packed content (silent corruption with
             * "FTP works fine" symptom). Unlinking here closes the gap
             * regardless of is_resume. ENOENT is the common case and
             * not an error. */
            if (!terminal && pool->worker_error == 0 && item.path) {
                char stale_tmp[1024];
                int  n = snprintf(stale_tmp, sizeof(stale_tmp),
                                  "%s.ps5up2-tmp", item.path);
                if (n > 0 && (size_t)n < sizeof(stale_tmp)) {
                    (void)unlink(stale_tmp);
                }
            }

            /* Fold per-item timings into the shared accumulator. */
            pthread_mutex_lock(&pool->lock);
            pool->t_open_us       += t_op;
            pool->t_ftruncate_us  += t_tr;
            pool->t_write_us      += t_wr;
            pool->t_close_us      += t_cl;
            pool->records_processed += 1;
            pool->open_retries    += local_open_retries;
            pool->write_retries   += local_write_retries;
            pthread_mutex_unlock(&pool->lock);
        }

        /* Free the reader-allocated buffers regardless of error state so the
         * queue always drains cleanly — the reader relies on that invariant. */
        free(item.path);
        if (item.data) free(item.data);
    }
}

/* Lazily start a pool attached to `entry`. Returns 0 on success, -1 on
 * pthread/allocation failure; on failure `entry->pack_pool` is left NULL so
 * the caller can fall back to the inline serial path. */
static int pack_pool_start(runtime_tx_entry_t *entry) {
    pack_worker_pool_t *pool;
    int i;
    if (!entry || entry->pack_pool) return 0;
    pool = (pack_worker_pool_t *)calloc(1, sizeof(*pool));
    if (!pool) return -1;
    pthread_mutex_init(&pool->lock, NULL);
    pthread_cond_init(&pool->cv_not_full, NULL);
    pthread_cond_init(&pool->cv_not_empty, NULL);
    pthread_cond_init(&pool->cv_drained, NULL);
    for (i = 0; i < (int)PS5UPLOAD2_PACK_WORKERS; i++) {
        if (create_worker_thread(&pool->tid[i], pack_worker_thread, pool) != 0) {
            /* Could not spawn all workers. Tear down the ones we did spawn
             * and fall back. Setting shutdown + broadcasting wakes the
             * already-running workers; they exit because queue is empty. */
            pool->shutdown = 1;
            pthread_cond_broadcast(&pool->cv_not_empty);
            {
                int j;
                for (j = 0; j < pool->tid_started; j++) {
                    pthread_join(pool->tid[j], NULL);
                }
            }
            pthread_mutex_destroy(&pool->lock);
            pthread_cond_destroy(&pool->cv_not_full);
            pthread_cond_destroy(&pool->cv_not_empty);
            pthread_cond_destroy(&pool->cv_drained);
            free(pool);
            return -1;
        }
        pool->tid_started += 1;
    }
    entry->pack_pool = pool;
    return 0;
}

/* Push one work item. Blocks if queue full. Returns 0 on success; returns
 * -1 only if the pool already has a sticky worker_error (in which case
 * ownership of path/data stays with the caller, which must free them). */
static int pack_pool_push(pack_worker_pool_t *pool, char *path,
                          unsigned char *data, uint32_t data_len) {
    if (!pool || !path) return -1;
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count >= (int)PS5UPLOAD2_PACK_QUEUE_DEPTH && !pool->worker_error) {
        pthread_cond_wait(&pool->cv_not_full, &pool->lock);
    }
    if (pool->worker_error) {
        pthread_mutex_unlock(&pool->lock);
        return -1;
    }
    pool->queue[pool->q_tail].path     = path;
    pool->queue[pool->q_tail].data     = data;
    pool->queue[pool->q_tail].data_len = data_len;
    pool->q_tail = (pool->q_tail + 1) % PS5UPLOAD2_PACK_QUEUE_DEPTH;
    pool->q_count += 1;
    pthread_cond_signal(&pool->cv_not_empty);
    pthread_mutex_unlock(&pool->lock);
    return 0;
}

/* Block until the queue is empty. Returns 0 if the pool is clean, -1 if a
 * worker has set the sticky error flag. The reader calls this at end-of-shard
 * so SHARD_ACK means "persisted", same contract as the serial code path. */
static int pack_pool_drain(pack_worker_pool_t *pool) {
    int err;
    if (!pool) return 0;
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count > 0) {
        pthread_cond_wait(&pool->cv_drained, &pool->lock);
    }
    err = pool->worker_error;
    pthread_mutex_unlock(&pool->lock);
    return err ? -1 : 0;
}

/* Drain + shutdown + join + sum timings into entry + free. Called from
 * runtime_release_tx_resources only; idempotent against NULL. */
static void pack_pool_teardown(runtime_tx_entry_t *entry) {
    pack_worker_pool_t *pool;
    int i;
    if (!entry || !entry->pack_pool) return;
    pool = (pack_worker_pool_t *)entry->pack_pool;

    /* Wait for in-flight work to finish — workers are reliable drain-ers
     * (they continue popping + freeing even after worker_error is set). */
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count > 0) {
        pthread_cond_wait(&pool->cv_drained, &pool->lock);
    }
    pool->shutdown = 1;
    pthread_cond_broadcast(&pool->cv_not_empty);
    pthread_mutex_unlock(&pool->lock);

    for (i = 0; i < pool->tid_started; i++) {
        pthread_join(pool->tid[i], NULL);
    }

    /* Fold pool timings into the entry so COMMIT_TX_ACK can report them. */
    entry->pack_records       += pool->records_processed;
    entry->pack_unlink_us     += pool->t_unlink_us;
    entry->pack_open_us       += pool->t_open_us;
    entry->pack_ftruncate_us  += pool->t_ftruncate_us;
    entry->pack_write_us      += pool->t_write_us;
    entry->pack_close_us      += pool->t_close_us;
    entry->pack_open_retries  += pool->open_retries;
    entry->pack_write_retries += pool->write_retries;

    if (pool->open_retries || pool->write_retries) {
        fprintf(stderr,
                "[payload2] pack pool teardown: %llu records, "
                "%llu open retries absorbed, %llu write retries absorbed\n",
                (unsigned long long)pool->records_processed,
                (unsigned long long)pool->open_retries,
                (unsigned long long)pool->write_retries);
    }

    pthread_mutex_destroy(&pool->lock);
    pthread_cond_destroy(&pool->cv_not_full);
    pthread_cond_destroy(&pool->cv_not_empty);
    pthread_cond_destroy(&pool->cv_drained);
    free(pool);
    entry->pack_pool = NULL;
}

/*
 * Read shard data from the socket, BLAKE3-hash it on the fly, and write it
 * to `path`. Uses raw POSIX `open(2)` + `write(2)` — bypassing stdio buffering
 * matches the legacy payload's high-throughput write pattern.
 *
 * `truncate`:
 *   - non-zero: open with O_WRONLY|O_CREAT|O_TRUNC — starts a fresh file
 *   - zero:     open with O_WRONLY|O_APPEND       — appends to existing file
 *
 * `preallocate_bytes`: if > 0 and the file is being truncated, `ftruncate(2)`
 * the file to that size before writing, so the kernel can lay out contiguous
 * blocks. Matches `payload/protocol.c` `handle_upload` large-file path.
 *
 * On success, if `out_digest` is non-NULL, it holds the BLAKE3-256 of the
 * shard contents — no re-read is needed for verification.
 *
 * Falls back to plain drain if `open` fails, so the socket is never left
 * in a bad read state.
 */
/*
 * Multi-file direct shard write. Each call: open the file, write `data_len`
 * bytes at `write_offset`, close.
 *
 * Offset model (changed in 2.16.0): the FIRST shard for a file opens with
 * O_TRUNC + posix_fallocate(preallocate_bytes), and the caller passes the
 * file's full expected size as `preallocate_bytes`. Subsequent shards open
 * O_WRONLY and lseek(SEEK_SET, write_offset) before writing. The pre-2.16.0
 * model used O_APPEND, which couldn't be combined with prealloc (fallocate
 * extends file size → O_APPEND writes land past the fallocated region) and
 * therefore left multi-GB files sparse-growing on UFS — the same metadata-
 * churn → throughput-collapse failure mode the single-file persistent writer
 * was fixed against in 2.2.29.
 */
static int runtime_write_shard_to_path(runtime_tx_entry_t *entry,
                                        const char *path,
                                        int truncate,
                                        uint64_t preallocate_bytes,
                                        uint64_t write_offset,
                                        int client_fd,
                                        uint64_t data_len,
                                        unsigned char *out_digest /* BLAKE3_OUT_LEN */) {
    unsigned char *bufs[2] = {NULL, NULL};
    int owns_bufs = 0; /* 1 = locally malloc'd (free here); 0 = borrowed from entry */
    int fd = -1;
    int flags;
    uint64_t remaining = data_len;
    uint64_t t_recv = 0;
    uint64_t t_write_wait = 0;
    uint64_t t_hash = 0;
    uint64_t t_func_start = now_us();
    uint64_t t_open_start = 0;
    uint64_t t_open_us = 0;
    uint64_t t_join_us = 0;
    uint64_t t_close_us = 0;
    blake3_hasher hasher;
    int hash_enabled = (out_digest != NULL);
    piped_writer_t pw;
    pthread_t writer_tid;
    int slot = 0;
    int rc_ret = 0;

    if (out_digest) memset(out_digest, 0, BLAKE3_OUT_LEN);
    if (hash_enabled) blake3_hasher_init(&hasher);

    if (data_len == 0) {
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        return 0;
    }

    /* Borrow the per-tx shared buffers (alloc-once, reuse across shards) when we
     * have an entry; otherwise fall back to a local alloc. This removes the
     * per-shard malloc/free that fragmented the heap on big multi-file folders
     * and that 4-way multi-stream would multiply. The entry's slot mutex is held
     * across this whole call, so a tx's two buffers have at most one user. */
    if (entry) {
        if (!entry->shard_io_buf[0])
            entry->shard_io_buf[0] = malloc(PS5UPLOAD2_SHARD_IO_BUF);
        if (!entry->shard_io_buf[1])
            entry->shard_io_buf[1] = malloc(PS5UPLOAD2_SHARD_IO_BUF);
        bufs[0] = (unsigned char *)entry->shard_io_buf[0];
        bufs[1] = (unsigned char *)entry->shard_io_buf[1];
    } else {
        bufs[0] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
        bufs[1] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
        owns_bufs = 1;
    }
    if (!bufs[0] || !bufs[1]) {
        if (owns_bufs) {
            free(bufs[0]);
            free(bufs[1]);
        }
        /* Borrowed buffers (if one allocated, one failed) stay on the entry and
         * are freed at tx release — don't free them here. */
        /* (2.9.0) Drain-then-FAIL — was return drain_shard_data(...) which
         * returns 0 on successful drain. Dispatcher treats 0 as "shard
         * persisted," ACKs the host, advances last_acked_shard, never
         * retries — destination file ends up with missing bytes while
         * the user sees "Upload complete." Silent corruption under PS5
         * RAM pressure. Always return -1 here so the caller treats it
         * as a hard write failure. */
        fprintf(stderr,
                "[payload2] shard buffer alloc failed (%zu B x2); draining + failing tx\n",
                (size_t)PS5UPLOAD2_SHARD_IO_BUF);
        if (entry) entry->last_io_errno = ENOMEM;
        (void)drain_shard_data(client_fd, data_len);
        return -1;
    }

    t_open_start = now_us();
    /* Truncate on first shard; subsequent shards rely on lseek below to
     * position into the (already preallocated) file. NEVER O_APPEND for the
     * multi-file path — see the function header. */
    flags = O_WRONLY | O_CREAT | (truncate ? O_TRUNC : 0);
    fd = open(path, flags, 0777);
    if (fd >= 0) (void)fchmod(fd, 0777);
    if (fd < 0) {
        if (entry) entry->last_io_errno = errno;
        /* Only free LOCALLY-owned buffers. Borrowed (entry) buffers persist on
         * the tx entry and are freed in runtime_release_tx_resources — freeing
         * them here left entry->shard_io_buf[] dangling → use-after-free on the
         * next shard + double-free at teardown (the huge-upload crash). */
        if (owns_bufs) {
            free(bufs[0]);
            free(bufs[1]);
        }
        /* Same drain-then-FAIL contract: open() failing while we still
         * try to drain the wire keeps framing recoverable but the tx
         * must abort, not silently succeed. */
        fprintf(stderr,
                "[payload2] shard open %s failed errno=%d; draining + failing tx\n",
                path, errno);
        (void)drain_shard_data(client_fd, data_len);
        return -1;
    }
    /* Position the fd at the correct write offset for this shard. For the
     * first shard of a file truncate==1 and write_offset==0 (open already left
     * fd at 0; lseek(0) is a harmless no-op). For subsequent shards we MUST
     * seek explicitly — the file was preallocated to its full size on shard 1
     * so SEEK_END would jump to that fallocated tail, not the actual data end.
     * lseek failure here means we'd corrupt the file by writing at the wrong
     * offset, so bail out (same drain-then-FAIL contract as open). */
    if (lseek(fd, (off_t)write_offset, SEEK_SET) == (off_t)-1) {
        int saved = errno;
        if (entry) entry->last_io_errno = saved;
        close(fd);
        if (owns_bufs) { /* borrowed buffers freed at tx release — see open-fail note */
            free(bufs[0]);
            free(bufs[1]);
        }
        fprintf(stderr,
                "[payload2] shard lseek %s to %llu failed errno=%d; draining + failing tx\n",
                path, (unsigned long long)write_offset, saved);
        (void)drain_shard_data(client_fd, data_len);
        return -1;
    }
    if (truncate && preallocate_bytes > 0) {
        /* Pre-allocate REAL blocks via posix_fallocate to avoid the
         * sustained-write throughput collapse pattern (60 MiB/s → 2-3
         * MiB/s a few minutes into a multi-GB upload) that plain
         * ftruncate causes on PS5 UFS by leaving the file sparse and
         * forcing per-write block-allocation + journal churn. Falls
         * back to ftruncate on filesystems where fallocate isn't
         * supported (exfat, fuse). Same migration as
         * runtime_write_shard_persistent below.
         *
         * (2.9.0) Differentiate fallback-on-unsupported from abort-on-
         * disk-full. ENOSPC means the destination volume CANNOT hold
         * the file — ftruncate would silently make it sparse, the
         * shard writes would proceed for the in-cache range, then
         * fail piecewise much later with no context. Abort the tx
         * front-loaded so the user sees "destination disk full" now,
         * not "open <random path>: ENOSPC" 50 GB into a 60 GB upload.
         * EINVAL / EOPNOTSUPP / ENOTSUP are the legitimate "filesystem
         * lacks fallocate" codes — those keep the ftruncate fallback.
         * Other returns (EBADF, EFBIG, EINTR, EIO) are unexpected;
         * log them and fall back too rather than bail, since the
         * sparse-file path is functionally correct for most of them. */
        int prealloc_rc = posix_fallocate(fd, 0, (off_t)preallocate_bytes);
        if (prealloc_rc == ENOSPC) {
            fprintf(stderr,
                    "[payload2] posix_fallocate %s: ENOSPC (need %lld B); aborting tx\n",
                    path, (long long)preallocate_bytes);
            if (entry) entry->last_io_errno = ENOSPC;
            close(fd);
            if (owns_bufs) { /* borrowed buffers freed at tx release — see open-fail note */
                free(bufs[0]);
                free(bufs[1]);
            }
            (void)drain_shard_data(client_fd, data_len);
            return -1;
        }
        if (prealloc_rc != 0) {
            fprintf(stderr,
                    "[payload2] posix_fallocate %s rc=%d (%s); falling back to ftruncate\n",
                    path, prealloc_rc, strerror(prealloc_rc));
            (void)ftruncate(fd, (off_t)preallocate_bytes);
        }
    }
    t_open_us = now_us() - t_open_start;

    /* Decide whether to spawn the double-buffered writer thread at all.
     * For small shards (typical PS5 game-dir file <64 KiB), the pthread
     * create/join cost dominates — measured at ~4–6 ms per shard on FreeBSD 11,
     * vs ~20 µs for the 4 KiB write itself. For large shards the overlap
     * between recv and write is still worth the thread overhead. */
    if (data_len < PS5UPLOAD2_PIPED_THREAD_MIN_BYTES) {
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            uint64_t t0 = now_us();
            if (recv_exact(client_fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            t_recv += (now_us() - t0);
            if (hash_enabled) {
                uint64_t th = now_us();
                blake3_hasher_update(&hasher, bufs[0], take);
                t_hash += (now_us() - th);
            }
            if (write_full(fd, bufs[0], take) != 0) {
                if (entry) entry->last_io_errno = errno;
                rc_ret = -1;
                break;
            }
            remaining -= (uint64_t)take;
        }
        {
            uint64_t t_close_start = now_us();
            close(fd);
            t_close_us = now_us() - t_close_start;
        }
        /* HOT PATH (sub-64KiB shard, the common case for folder uploads).
         * Borrowed (entry) buffers MUST persist for the next shard — freeing
         * them here is the root cause of the huge-upload crash (UAF next shard +
         * double-free at tx release). Only free locally-owned buffers. */
        if (owns_bufs) {
            free(bufs[0]);
            free(bufs[1]);
        }
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (entry && rc_ret == 0) {
            entry->recv_us       += t_recv;
            entry->open_us       += t_open_us;
            entry->close_us      += t_close_us;
            entry->hash_us       += t_hash;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return rc_ret;
    }

    /* Set up double-buffered writer thread so recv and write overlap. */
    memset(&pw, 0, sizeof(pw));
    pw.fd = fd;
    pw.slot[0].buf = bufs[0];
    pw.slot[1].buf = bufs[1];
    pthread_mutex_init(&pw.lock, NULL);
    pthread_cond_init(&pw.cv_full[0], NULL);
    pthread_cond_init(&pw.cv_full[1], NULL);
    pthread_cond_init(&pw.cv_empty[0], NULL);
    pthread_cond_init(&pw.cv_empty[1], NULL);
    if (create_worker_thread(&writer_tid, piped_writer_thread, &pw) != 0) {
        /* Fall back to single-buffered in-thread writes if thread create fails. */
        pthread_mutex_destroy(&pw.lock);
        pthread_cond_destroy(&pw.cv_full[0]);
        pthread_cond_destroy(&pw.cv_full[1]);
        pthread_cond_destroy(&pw.cv_empty[0]);
        pthread_cond_destroy(&pw.cv_empty[1]);
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            if (recv_exact(client_fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            if (hash_enabled) blake3_hasher_update(&hasher, bufs[0], take);
            if (write_full(fd, bufs[0], take) != 0) {
                if (entry) entry->last_io_errno = errno;
                rc_ret = -1;
                break;
            }
            remaining -= (uint64_t)take;
        }
        close(fd);
        if (owns_bufs) {
            free(bufs[0]);
            free(bufs[1]);
        }
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        return rc_ret;
    }

    while (remaining > 0) {
        uint64_t t0, t1, t2;
        size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                        ? PS5UPLOAD2_SHARD_IO_BUF
                        : (size_t)remaining;
        /* Wait for the target slot to be free (writer has finished draining it). */
        t0 = now_us();
        pthread_mutex_lock(&pw.lock);
        while (pw.slot[slot].len > 0 && !pw.writer_error) {
            pthread_cond_wait(&pw.cv_empty[slot], &pw.lock);
        }
        if (pw.writer_error) {
            pthread_mutex_unlock(&pw.lock);
            rc_ret = -1;
            break;
        }
        pthread_mutex_unlock(&pw.lock);
        t1 = now_us();
        t_write_wait += (t1 - t0);

        if (recv_exact(client_fd, pw.slot[slot].buf, take) != 0) {
            rc_ret = -1;
            break;
        }
        t2 = now_us();
        t_recv += (t2 - t1);

        if (hash_enabled) {
            uint64_t t_hash_start = now_us();
            blake3_hasher_update(&hasher, pw.slot[slot].buf, take);
            t_hash += (now_us() - t_hash_start);
        }

        /* Hand slot to writer. */
        pthread_mutex_lock(&pw.lock);
        pw.slot[slot].len = (ssize_t)take;
        pthread_cond_signal(&pw.cv_full[slot]);
        pthread_mutex_unlock(&pw.lock);
        slot = 1 - slot;
        remaining -= (uint64_t)take;
    }

    /* Signal shutdown on the next slot the writer will pick up. */
    pthread_mutex_lock(&pw.lock);
    /* Wait for the sentinel slot to be free before writing it. */
    while (pw.slot[slot].len > 0 && !pw.writer_error) {
        pthread_cond_wait(&pw.cv_empty[slot], &pw.lock);
    }
    pw.slot[slot].len = -1;
    pthread_cond_signal(&pw.cv_full[slot]);
    pthread_mutex_unlock(&pw.lock);

    {
        uint64_t t_join_start = now_us();
        pthread_join(writer_tid, NULL);
        t_join_us = now_us() - t_join_start;
    }
    if (pw.writer_error) rc_ret = -1;

    {
        uint64_t t_close_start = now_us();
        close(fd);
        t_close_us = now_us() - t_close_start;
    }
    pthread_mutex_destroy(&pw.lock);
    pthread_cond_destroy(&pw.cv_full[0]);
    pthread_cond_destroy(&pw.cv_full[1]);
    pthread_cond_destroy(&pw.cv_empty[0]);
    pthread_cond_destroy(&pw.cv_empty[1]);
    if (owns_bufs) {
        free(bufs[0]);
        free(bufs[1]);
    }
    if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
    if (entry && rc_ret == 0) {
        entry->recv_us       += t_recv;
        /* `write_us` now counts time the producer was blocked waiting on the
         * writer — roughly the *excess* write time over recv time. A healthy
         * pipeline has write_us small; if it's large, writes are the limit. */
        entry->write_us      += t_write_wait;
        entry->open_us       += t_open_us;
        entry->join_us       += t_join_us;
        entry->close_us      += t_close_us;
        entry->hash_us       += t_hash;
        entry->shard_func_us += (now_us() - t_func_start);
    }
    return rc_ret;
}

/*
 * Single-file direct-mode persistent-fd shard write path.
 *
 * The first shard opens `entry->tmp_path` with O_TRUNC + ftruncate
 * preallocation (if total_bytes is known), stashes the fd on `entry`, and
 * — if the expected total tx size is large enough to amortise pthread
 * overhead — spawns a long-lived writer thread via direct_writer_start().
 *
 * Every shard pumps recv→hash→slot exactly like the large-shard inner loop
 * of runtime_write_shard_to_path, but without spawning/joining/closing.
 *
 * On write-path I/O failure the caller receives -1; it must then call
 * runtime_abort_tx_fatal (which goes through runtime_release_tx_resources
 * and tears the writer down) — the persistent writer is not safe to reuse
 * after writer_error is set.
 *
 * Small single-file txs (total_bytes below the threshold) keep the fd
 * persistent but do sync writes inline — the writer-thread overhead isn't
 * worth it for a few shards' worth of bytes, and fd reuse still saves the
 * 4× open+close overhead per shard that the old per-shard path paid.
 */
static int runtime_write_shard_persistent(runtime_tx_entry_t *entry,
                                           int client_fd,
                                           uint64_t data_len,
                                           int is_first_shard,
                                           uint64_t preallocate_bytes,
                                           unsigned char *out_digest) {
    uint64_t remaining = data_len;
    uint64_t t_recv = 0;
    uint64_t t_write_wait = 0;
    uint64_t t_hash = 0;
    uint64_t t_func_start = now_us();
    uint64_t t_open_us = 0;
    uint64_t t_close_us = 0;
    blake3_hasher hasher;
    int hash_enabled = (out_digest != NULL);
    int rc_ret = 0;
    direct_writer_handle_t *h = NULL;
    int use_writer_thread = 0;
    unsigned char *sync_buf = NULL;

    if (!entry) return -1;
    if (out_digest) memset(out_digest, 0, BLAKE3_OUT_LEN);
    if (hash_enabled) blake3_hasher_init(&hasher);

    /* Open the tmp file when not already open. Two cases:
     *
     *   1. Fresh first shard (is_first_shard=1, direct_fd_open=0):
     *      open with O_TRUNC + ftruncate-to-total_bytes, optionally
     *      spawn the writer thread.
     *
     *   2. Resume scenario (is_first_shard=0, direct_fd_open=0):
     *      pre-2.2.28 this case fell into the sync-write path with
     *      direct_fd=-1 → write_full(-1, ...) failed EBADF and resume
     *      of single-file direct uploads was broken end-to-end. Fix:
     *      reopen with O_APPEND so every write lands at end-of-file,
     *      preserving the bytes from shards 1..N already on disk
     *      (ephemeral release intentionally keeps the tmp file). We
     *      do NOT spawn the writer thread on resume — the producer/
     *      consumer pattern reuses an in-memory state machine that
     *      doesn't survive ephemeral teardown, and a one-shot resume
     *      is rarely throughput-critical anyway (sync fd write is
     *      enough to drain a few MB of pending data). */
    if (!entry->direct_fd_open) {
        int flags = O_WRONLY | O_CREAT;
        int is_resume_open = !is_first_shard;
        if (is_resume_open) {
            flags |= O_APPEND;
        } else {
            flags |= O_TRUNC;
        }
        uint64_t t_open_start = now_us();
        int fd = open(entry->tmp_path, flags, 0777);
        if (fd >= 0) (void)fchmod(fd, 0777);
        if (fd < 0) {
            /* (2.9.0) Drain-then-FAIL — see runtime_write_shard_to_path
             * for the silent-corruption-bug context. Was returning
             * drain_shard_data() rc directly; on a 0 the caller thinks
             * the shard persisted and ACKs the host. Always return -1
             * so the tx aborts. */
            fprintf(stderr, "[payload2] direct persistent: open %s failed errno=%d\n",
                    entry->tmp_path, errno);
            entry->last_io_errno = errno;
            (void)drain_shard_data(client_fd, data_len);
            return -1;
        }
        if (!is_resume_open && preallocate_bytes > 0) {
            /* Pre-allocate the destination's blocks UP FRONT via
             * posix_fallocate. The pre-2.2.29 path used ftruncate
             * which on PS5 UFS creates a sparse file — every write
             * to a previously-unallocated block has to bring in a
             * fresh block, journal the bitmap update, and dirty an
             * indirect block. For multi-GB uploads (game images,
             * 30+ GB UFS .ffpkgs) the dirty-buffer pressure
             * eventually trips the kernel's throttle and per-shard
             * throughput collapses from ~60 MiB/s to ~2-3 MiB/s
             * a few minutes into the transfer. posix_fallocate
             * pre-allocates all blocks in one batch — subsequent
             * shard writes only update content, no metadata churn.
             *
             * Falls back to ftruncate if fallocate isn't supported
             * by the destination filesystem (exfat / fuse / NFS),
             * matching the pattern cp_rf already uses. The fallback
             * preserves the file-size semantic even on exfat where
             * sparse-file cost is lower (FAT-family doesn't journal
             * metadata block-by-block).
             *
             * (2.9.0) Differentiate ENOSPC from "unsupported." See
             * runtime_write_shard_to_path for the same rationale —
             * ENOSPC at the front of a 60 GB upload is information
             * the user needs NOW, not 50 GB later via a misleading
             * "open failed" message piecewise. */
            int prealloc_rc = posix_fallocate(fd, 0, (off_t)preallocate_bytes);
            if (prealloc_rc == ENOSPC) {
                fprintf(stderr,
                        "[payload2] direct persistent posix_fallocate %s: ENOSPC "
                        "(need %lld B); aborting tx\n",
                        entry->tmp_path, (long long)preallocate_bytes);
                entry->last_io_errno = ENOSPC;
                close(fd);
                (void)drain_shard_data(client_fd, data_len);
                return -1;
            }
            if (prealloc_rc != 0) {
                fprintf(stderr,
                        "[payload2] direct persistent posix_fallocate %s rc=%d (%s); "
                        "falling back to ftruncate\n",
                        entry->tmp_path, prealloc_rc, strerror(prealloc_rc));
                (void)ftruncate(fd, (off_t)preallocate_bytes);
            }
        }
        /* posix_fadvise(SEQUENTIAL) was experimented with here; measured
         * neutral-to-slightly-negative on the e1000 lab NIC and produced no
         * observable benefit on huge-file throughput (already NIC-bound).
         * Removed to avoid any chance of unexpected kernel-scheduler
         * interactions. See bench/reports/2026-04-18T01* for the A/B data. */
        entry->direct_fd      = fd;
        entry->direct_fd_open = 1;
        t_open_us = now_us() - t_open_start;

        /* Spawn the writer thread only on the fresh-first-shard path
         * AND only if the tx is big enough to amortise pthread
         * create/join cost. Resume-open uses sync writes (see comment
         * above the open). */
        if (!is_resume_open &&
            entry->total_bytes >= PS5UPLOAD2_PIPED_THREAD_MIN_BYTES) {
            if (direct_writer_start(entry, fd) != 0) {
                /* Fall back to sync writes on a persistent fd — slower but
                 * functionally correct. Don't close fd; entry owns it. */
                fprintf(stderr, "[payload2] direct persistent: writer start failed, sync fallback\n");
            }
        }
    }

    if (entry->direct_writer) {
        h = (direct_writer_handle_t *)entry->direct_writer;
        use_writer_thread = 1;
    }

    if (data_len == 0) {
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (entry) {
            entry->open_us       += t_open_us;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return 0;
    }

    if (!use_writer_thread) {
        /* Sync write path — one reusable buffer, persistent fd. */
        sync_buf = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
        if (!sync_buf) {
            /* (2.9.0) Drain-then-FAIL — see runtime_write_shard_to_path
             * for context. The pre-2.9.0 code path returned 0 to the
             * dispatcher on a successful drain, leading to silent
             * shard-loss corruption under PS5 RAM pressure. */
            fprintf(stderr,
                    "[payload2] direct persistent: sync_buf alloc failed (%zu B); failing tx\n",
                    (size_t)PS5UPLOAD2_SHARD_IO_BUF);
            entry->last_io_errno = ENOMEM;
            (void)drain_shard_data(client_fd, data_len);
            return -1;
        }
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            uint64_t t0 = now_us();
            if (recv_exact(client_fd, sync_buf, take) != 0) { rc_ret = -1; break; }
            t_recv += (now_us() - t0);
            if (hash_enabled) {
                uint64_t th = now_us();
                blake3_hasher_update(&hasher, sync_buf, take);
                t_hash += (now_us() - th);
            }
            if (write_full(entry->direct_fd, sync_buf, take) != 0) {
                entry->last_io_errno = errno;
                rc_ret = -1; break;
            }
            remaining -= (uint64_t)take;
        }
        free(sync_buf);
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (rc_ret == 0) {
            entry->recv_us       += t_recv;
            entry->open_us       += t_open_us;
            entry->hash_us       += t_hash;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return rc_ret;
    }

    /* Persistent writer-thread path — reuse pw.slot[direct_slot] across
     * shards. The writer's internal slot counter and our direct_slot both
     * advance 0→1→0 in lockstep, so as long as neither side re-initializes
     * they stay aligned across shards. */
    while (remaining > 0) {
        int slot = entry->direct_slot;
        uint64_t t0, t1, t2;
        size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                        ? PS5UPLOAD2_SHARD_IO_BUF
                        : (size_t)remaining;
        t0 = now_us();
        pthread_mutex_lock(&h->pw.lock);
        while (h->pw.slot[slot].len > 0 && !h->pw.writer_error) {
            pthread_cond_wait(&h->pw.cv_empty[slot], &h->pw.lock);
        }
        if (h->pw.writer_error) {
            pthread_mutex_unlock(&h->pw.lock);
            rc_ret = -1;
            break;
        }
        pthread_mutex_unlock(&h->pw.lock);
        t1 = now_us();
        t_write_wait += (t1 - t0);

        if (recv_exact(client_fd, h->pw.slot[slot].buf, take) != 0) {
            rc_ret = -1;
            break;
        }
        t2 = now_us();
        t_recv += (t2 - t1);

        if (hash_enabled) {
            uint64_t t_hash_start = now_us();
            blake3_hasher_update(&hasher, h->pw.slot[slot].buf, take);
            t_hash += (now_us() - t_hash_start);
        }

        pthread_mutex_lock(&h->pw.lock);
        h->pw.slot[slot].len = (ssize_t)take;
        pthread_cond_signal(&h->pw.cv_full[slot]);
        pthread_mutex_unlock(&h->pw.lock);
        entry->direct_slot = 1 - slot;
        remaining -= (uint64_t)take;
    }

    if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
    /* Note: close_us stays 0 for persistent path — fd only closes at COMMIT. */
    (void)t_close_us;
    if (rc_ret == 0) {
        entry->recv_us       += t_recv;
        entry->write_us      += t_write_wait;
        entry->open_us       += t_open_us;
        entry->hash_us       += t_hash;
        entry->shard_func_us += (now_us() - t_func_start);
    }
    return rc_ret;
}

/* Spool-path wrapper (kept for fallback when direct mode is not engaged). */
static int runtime_write_shard_data(runtime_tx_entry_t *entry,
                                     uint64_t shard_seq,
                                     int client_fd,
                                     uint64_t data_len,
                                     unsigned char *out_digest) {
    char spool_dir[512];
    char path[512];
    if (!entry) return drain_shard_data(client_fd, data_len);
    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);
    if (ensure_dir(spool_dir) != 0) {
        return drain_shard_data(client_fd, data_len);
    }
    snprintf(path, sizeof(path), "%s/%llu",
             spool_dir, (unsigned long long)shard_seq);
    /* Spool shard file is always a fresh single-shard write — truncate +
     * preallocate `data_len`, write at offset 0. */
    return runtime_write_shard_to_path(entry, path, 1, data_len,
                                        /*write_offset=*/0,
                                        client_fd, data_len, out_digest);
}

/* ── Apply + cleanup ──────────────────────────────────────────────────────────── */

/*
 * mkdir -p for all ancestor directories of `path`.
 * Does not create `path` itself (assumes it is a file, not a dir).
 */
static int ensure_parent_dir(const char *path) {
    char tmp[512];
    char *p = NULL;
    size_t len = 0;
    if (!path || !*path) return -1;
    len = strlen(path);
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);
    /* Find the last slash to isolate the directory part. */
    p = tmp + len - 1;
    while (p > tmp && *p != '/') p--;
    if (p == tmp) return 0; /* no parent directory */
    *p = '\0'; /* truncate to parent path */
    /* Walk forward and mkdir each component. */
    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
    return 0;
}


/*
 * ensure_parent_dir() but backed by a single-slot cache on the tx entry.
 * For N files in the same directory (common in game dirs — 10s of siblings
 * per dir), only the first pays the mkdir walk. Subsequent calls just
 * strncmp the cached dirname against the new path's dirname.
 */
static int ensure_parent_dir_cached(runtime_tx_entry_t *entry,
                                     const char *path) {
    const char *slash;
    size_t dir_len;
    if (!path || !*path) return -1;
    /* Locate the dirname boundary without building a temp buffer. */
    slash = path + strlen(path);
    while (slash > path && *(slash - 1) != '/') slash -= 1;
    /* slash now points one past the last '/', so dir = [path, slash). */
    if (slash == path) return 0; /* no parent */
    dir_len = (size_t)((slash - 1) - path);
    if (dir_len == 0) return 0; /* root */
    if (entry && dir_len < sizeof(entry->last_parent_dir)) {
        if (entry->last_parent_dir[0] &&
            strlen(entry->last_parent_dir) == dir_len &&
            memcmp(entry->last_parent_dir, path, dir_len) == 0) {
            return 0; /* hit */
        }
    }
    if (ensure_parent_dir(path) != 0) return -1;
    if (entry && dir_len < sizeof(entry->last_parent_dir)) {
        memcpy(entry->last_parent_dir, path, dir_len);
        entry->last_parent_dir[dir_len] = '\0';
    }
    return 0;
}

/*
 * Manifest file entry parsed from the "files" JSON array.
 */
struct manifest_file_entry {
    char path[512];
    uint64_t shard_start; /* 1-based, first shard for this file */
    uint64_t shard_count; /* number of shards carrying this file */
    uint64_t size;        /* expected file size in bytes (0 if absent) */
};
typedef struct manifest_file_entry manifest_file_entry_t;

/*
 * Compact, parsed-once representation of a manifest file entry.
 * `path_offset` / `path_len` point into the owning entry's `manifest_blob`
 * heap buffer, so the full manifest JSON must outlive this index.
 * 32 bytes per entry — 223k files → 7.1 MiB of index on PS5 heap.
 */
struct manifest_index_entry {
    uint64_t shard_start;   /* 1-based, first shard carrying this file */
    uint32_t shard_count;   /* >= 1 */
    uint32_t path_offset;   /* offset into manifest_blob */
    uint32_t path_len;      /* length of the path in bytes */
    uint32_t _reserved;
    uint64_t size;          /* file size in bytes (informational) */
    /* IN-MEMORY ONLY (not journaled). Per-tx running offset for the next
     * write into this file's `.ps5up2-tmp`. Lets the multi-file direct write
     * path posix_fallocate(size) on the first shard + write each subsequent
     * shard at the correct byte offset, instead of using O_APPEND. The old
     * O_APPEND model couldn't preallocate (fallocate extends file size, but
     * O_APPEND writes go to EOF) and caused UFS throughput collapse on multi-
     * GB files mid-transfer — the same failure mode the single-file persistent
     * writer was fixed for in 2.2.29. Reset to 0 on every fresh BEGIN_TX
     * (calloc'd by build_manifest_index) and on resume-rebuild; the resume
     * reconcile clamps the cursor to a file boundary so a re-sent file always
     * starts at offset 0 anyway. */
    uint64_t bytes_written;
};

/*
 * Parse "files":[ … ] once from `blob` into a heap-allocated index sorted by
 * shard_start. Writes `*out_index` / `*out_count` on success. On any failure
 * (no files array, bad JSON, entry count > limit, OOM) frees partial work
 * and returns -1.
 *
 * `blob` is consumed by offset (not copied) — the caller must keep it alive
 * as long as the returned index is used.
 */
static int build_manifest_index(const char *blob, size_t blob_len,
                                uint64_t expected_count,
                                manifest_index_entry_t **out_index,
                                uint64_t *out_count) {
    const char *p = NULL;
    const char *arr_start = NULL;
    manifest_index_entry_t *idx = NULL;
    uint64_t cap = 0;
    uint64_t n = 0;

    if (!blob || !out_index || !out_count) return -1;
    *out_index = NULL;
    *out_count = 0;
    if (expected_count == 0 || expected_count > PS5UPLOAD2_MAX_MANIFEST_FILES) {
        return -1;
    }
    /* `expected_count` (the client's `file_count`) sizes the calloc below.
     * The smallest possible manifest entry is several bytes of JSON, so a
     * blob of length `blob_len` cannot describe more than `blob_len`
     * entries — a count larger than that is malformed (or a crafted
     * "tiny body, huge count" request aimed at reserving 40 MB). Reject it
     * so the allocation stays proportional to data actually transmitted. */
    if (blob_len > 0 && expected_count > (uint64_t)blob_len) {
        return -1;
    }

    p = strstr(blob, "\"files\":");
    if (!p) return -1;
    p = strchr(p, '[');
    if (!p) return -1;
    arr_start = p + 1;

    /* Allocate to exact expected count so we never realloc mid-parse. If the
     * JSON has fewer entries than expected, we shrink `n` at the end; more
     * entries than expected is a malformed manifest. */
    cap = expected_count;
    idx = (manifest_index_entry_t *)calloc((size_t)cap, sizeof(*idx));
    if (!idx) return -1;

    p = arr_start;
    while (n < cap) {
        const char *obj_start;
        const char *obj_end;
        uint64_t s_start;
        uint64_t s_count;
        uint64_t s_size;
        const char *path_key;
        const char *path_value_start;
        const char *path_value_end;
        size_t plen;

        obj_start = strchr(p, '{');
        if (!obj_start) break;
        /* Brace-aware object end: a file path containing a literal '}'
         * (valid, and emitted unescaped by the engine's serializer) must
         * not be mistaken for the object terminator. See json_object_end. */
        obj_end = json_object_end(obj_start, blob_len ? blob + blob_len : NULL);
        if (!obj_end) { free(idx); return -1; }

        /* Extract shard_start / shard_count from within this object only.
         * Per-object isolation matters: a malformed/crafted manifest where
         * one object's `shard_start` is missing would otherwise cause
         * extract_json_uint64_field's unbounded strstr to skip ahead and
         * read the NEXT object's value, silently aliasing the two. We
         * make a NUL-terminated stack copy of just the object slice so
         * the helper's strstr can't escape past obj_end. */
        {
            char obj_buf[1024];
            size_t obj_len = (size_t)(obj_end - obj_start) + 1; /* include '}' */
            if (obj_len >= sizeof(obj_buf)) {
                /* Object larger than our stack buffer — no real manifest
                 * has 1KB+ per-file metadata (~150B typical, includes
                 * up-to-512B path). Reject as malformed. */
                free(idx); return -1;
            }
            memcpy(obj_buf, obj_start, obj_len);
            obj_buf[obj_len] = '\0';
            s_start = extract_json_uint64_field(obj_buf, "shard_start");
            s_count = extract_json_uint64_field(obj_buf, "shard_count");
            /* `size` MUST be read from the bounded object slice too. The
             * unbounded variant (extract_json_uint64_field(obj_start, …))
             * silently reads the NEXT object's `size` when this object
             * lacks one — and `idx[].size` is the value the COMMIT-time
             * integrity check trusts to detect a short/corrupt file, so a
             * stale read there could validate a truncated file against the
             * wrong size and let corruption pass as success. Same isolation
             * rationale as shard_start/shard_count above. */
            s_size  = extract_json_uint64_field(obj_buf, "size");
            if (s_count == 0) s_count = 1;
        }
        if (s_start == 0) { free(idx); return -1; }
        /* Reject ranges that would overflow at the lookup site
         * (`shard_seq - s >= ec` is overflow-safe, but a sane manifest
         * never has shard_start anywhere near UINT64_MAX so a high
         * value indicates a malformed/malicious manifest — reject
         * outright). The sender's own engine cap is well below 2^32. */
        if (s_start > UINT64_MAX - s_count) { free(idx); return -1; }

        /* Locate "path":"..." inside this object. We find the opening quote
         * after the key, then the closing quote. The path must not contain
         * escaped quotes — the writer never emits those. */
        path_key = strstr(obj_start, "\"path\":\"");
        if (!path_key || path_key >= obj_end) { free(idx); return -1; }
        path_value_start = path_key + strlen("\"path\":\"");
        path_value_end = path_value_start;
        path_value_end = json_string_end(path_value_start, obj_end);
        if (!path_value_end) { free(idx); return -1; }
        plen = (size_t)(path_value_end - path_value_start);
        if (plen == 0 || plen >= 512) { free(idx); return -1; }

        /* Validate every manifest file path against the writable-roots
         * allowlist (is_path_allowed also rejects ".." components and any
         * absolute path outside the allowed roots, even for not-yet-created
         * destinations). BEGIN_TX validates dest_root and handle_packed_shard
         * validates packed per-record paths, but the multi-file DIRECT writer
         * (handle_stream_shard) and the SPOOL apply (runtime_apply_spool)
         * consume these `files[].path` entries straight from the manifest —
         * without this, a crafted manifest with an absolute path like
         * /system_ex/... would let any LAN client write outside the allowed
         * roots through the elevated payload. Reject the whole tx on any bad
         * path so a single poisoned entry can't be partially applied. */
        {
            char pbuf[512];
            memcpy(pbuf, path_value_start, plen);
            pbuf[plen] = '\0';
            if (!is_path_allowed(pbuf)) {
                free(idx);
                return -1;
            }
        }

        idx[n].shard_start = s_start;
        idx[n].shard_count = (uint32_t)s_count;
        idx[n].path_offset = (uint32_t)(path_value_start - blob);
        idx[n].path_len    = (uint32_t)plen;
        idx[n].size        = s_size;
        n += 1;

        /* Ordering tolerance: we require only non-decreasing shard_start.
         * Packed shards deliberately share a shard_start across many file
         * entries (all records live in the same STREAM_SHARD frame), so
         * strict inequality would reject valid manifests. Non-packed shards
         * whose ranges genuinely overlap would be a host bug — but packed
         * shards bypass the index entirely in handle_stream_shard, so a
         * misrouted lookup there is harmless. */
        if (n > 1 && idx[n - 1].shard_start < idx[n - 2].shard_start) {
            free(idx);
            return -1;
        }

        p = obj_end + 1;
        /* Allow blob_len to bound the scan (defence-in-depth vs. missing NUL). */
        if (blob_len > 0 && (size_t)(p - blob) >= blob_len) break;
    }

    if (n == 0) { free(idx); return -1; }

    *out_index = idx;
    *out_count = n;
    return 0;
}

/*
 * Binary-search the sorted index for the file entry that owns `shard_seq`.
 * Returns 0 and fills `out` (including copying the path out of `blob` into
 * `out->path`) on success; -1 if no entry covers the shard.
 *
 * Both arithmetic comparisons are written so they cannot overflow even
 * against a maliciously crafted manifest where shard_start is near
 * UINT64_MAX. `shard_seq - s` is only computed under `shard_seq >= s`,
 * and the matched range is bounded with a path-offset/length check
 * against `blob_len` to prevent an out-of-bounds memcpy.
 */
static int lookup_manifest_index(const manifest_index_entry_t *idx,
                                 uint64_t count,
                                 const char *blob,
                                 size_t blob_len,
                                 uint64_t shard_seq,
                                 manifest_file_entry_t *out) {
    uint64_t lo = 0;
    uint64_t hi;
    if (!idx || count == 0 || !blob || !out) return -1;
    hi = count;
    while (lo < hi) {
        uint64_t mid = lo + (hi - lo) / 2;
        uint64_t s  = idx[mid].shard_start;
        uint64_t ec = idx[mid].shard_count > 0 ? idx[mid].shard_count : 1;
        if (shard_seq < s) {
            hi = mid;
        } else if (shard_seq - s >= ec) {
            /* shard_seq >= s ensures the subtraction is safe; this
             * replaces the old `shard_seq >= s + ec` which could wrap
             * if a malicious manifest set shard_start near UINT64_MAX. */
            lo = mid + 1;
        } else {
            size_t plen = idx[mid].path_len;
            uint32_t poff = idx[mid].path_offset;
            if (plen >= sizeof(out->path)) return -1;
            /* Bounds-check path_offset + plen against the manifest
             * blob length so a crafted index can't cause an
             * out-of-bounds read. The build_manifest_index parser
             * keeps these inside the blob in normal operation, but
             * defence-in-depth guards against a future bug or an
             * already-corrupted manifest_index allocation. */
            if (blob_len > 0 &&
                ((uint64_t)poff > blob_len ||
                 (uint64_t)poff + (uint64_t)plen > blob_len)) {
                return -1;
            }
            memset(out, 0, sizeof(*out));
            if (json_copy_unescaped_string(blob + poff, blob + poff + plen,
                                           out->path, sizeof(out->path)) != 0) {
                return -1;
            }
            out->shard_start = s;
            out->shard_count = ec;
            out->size        = idx[mid].size;
            return 0;
        }
    }
    return -1;
}

/*
 * Free the heap-owned manifest state on a tx entry. Safe to call repeatedly
 * and on entries that never had a manifest (everything is NULL-checked).
 * Called on every terminal transition and on eviction of a terminal slot.
 */
static void runtime_release_tx_resources(runtime_tx_entry_t *entry) {
    if (!entry) return;
    /* Persistent direct-write cleanup: discards any in-flight writer +
     * closes the fd if still open. This is called from every terminal
     * transition (COMMIT after rename, ABORT, takeover/shutdown fan-out,
     * abort_tx_fatal). COMMIT's success path has already renamed the tmp
     * to dest_root before we get here, so unlink below is a no-op there.
     * For every other path, the tmp is stale and must be removed so
     * `.ps5up2-tmp` files do not survive a terminal transition. */
    direct_writer_discard(entry);
    /* Packed-shard worker pool teardown: drain + shutdown + join; fold
     * per-worker timings into the entry so the next COMMIT_TX_ACK can
     * report them. Must happen BEFORE the tmp unlink below so in-flight
     * worker writes cannot race with our cleanup. */
    pack_pool_teardown(entry);
    if (entry->direct_mode && entry->tmp_path[0]) {
        (void)unlink(entry->tmp_path);
    }
    if (entry->manifest_index) {
        free(entry->manifest_index);
        entry->manifest_index = NULL;
    }
    entry->manifest_index_count = 0;
    if (entry->manifest_blob) {
        free(entry->manifest_blob);
        entry->manifest_blob = NULL;
    }
    entry->manifest_blob_len = 0;
    if (entry->shard_log_fp) {
        fclose((FILE *)entry->shard_log_fp);
        entry->shard_log_fp = NULL;
    }
    /* Free the per-tx shared multi-file shard buffers (see runtime.h). NULLed so
     * a reused slot re-allocs lazily. free(NULL) is a no-op. */
    free(entry->shard_io_buf[0]);
    free(entry->shard_io_buf[1]);
    entry->shard_io_buf[0] = NULL;
    entry->shard_io_buf[1] = NULL;
    entry->last_parent_dir[0] = '\0';
}

/*
 * Ephemeral-release: clear only thread-bound / heap state that the next
 * BEGIN_TX (resume) cannot rebuild from the journal. Specifically:
 *   - drain + destroy the persistent direct writer
 *   - drain + destroy the packed-shard worker pool (folds timings back in)
 *   - close the shard-log fd
 * Preserves:
 *   - entry->tmp_path on disk (holds partial shard data)
 *   - entry->manifest_blob / manifest_index (rebuilding on resume is
 *     wasted work at best and an OOM risk at worst on constrained heaps)
 *   - shards_received / bytes_received / dest_root (resume needs these)
 *
 * Used by the interrupt paths (connection drop, takeover, shutdown). Matches
 * the pattern used by `runtime_release_tx_resources` minus the tmp unlink
 * and heap free. */
static void runtime_release_tx_resources_ephemeral(runtime_tx_entry_t *entry) {
    if (!entry) return;
    direct_writer_discard(entry);
    pack_pool_teardown(entry);
    if (entry->shard_log_fp) {
        fclose((FILE *)entry->shard_log_fp);
        entry->shard_log_fp = NULL;
    }
    /* Free the shared shard buffers on interrupt too — a resume re-allocs them
     * lazily on its first non-packed shard. Reclaims 8 MiB/tx promptly. */
    free(entry->shard_io_buf[0]);
    free(entry->shard_io_buf[1]);
    entry->shard_io_buf[0] = NULL;
    entry->shard_io_buf[1] = NULL;
    entry->last_parent_dir[0] = '\0';
}

/*
 * Mark a tx as fatally aborted from a mid-lifecycle error path.
 * Decrements active_transactions, sets state to "aborted", flushes the JSON
 * record, releases manifest heap state, saves summary. Balances the
 * `active_transactions += 1` that BEGIN_TX did up front so a run of broken
 * requests doesn't leak the active counter.
 *
 * Safe to call on an entry that may or may not already have manifest state.
 */
static void runtime_abort_tx_fatal(runtime_state_t *state,
                                    runtime_tx_entry_t *entry) {
    if (!state || !entry) return;
    pthread_mutex_lock(&state->state_mtx);
    if (state->active_transactions > 0) state->active_transactions -= 1;
    pthread_mutex_unlock(&state->state_mtx);
    snprintf(entry->state, sizeof(entry->state), "aborted");
    (void)runtime_flush_tx_record(state, entry);
    runtime_release_tx_resources(entry);
    (void)runtime_save_tx_state(state);
}

/* (manifest_find_file_for_shard removed — superseded by build_manifest_index +
 * lookup_manifest_index, which are O(log N) per shard instead of O(N).) */

/*
 * Parse the Nth (0-based) file entry from a JSON "files" array.
 * Extracts "path", "shard_start", and "shard_count".
 * Returns 0 on success, -1 if the Nth entry is not found.
 */
static int manifest_get_nth_file_path(const char *json, uint64_t n,
                                       manifest_file_entry_t *out) {
    const char *p = json;
    uint64_t idx = 0;
    /* Find "files":[ then scan for the Nth object. */
    p = strstr(p, "\"files\":");
    if (!p) return -1;
    p = strchr(p, '[');
    if (!p) return -1;
    p++;
    for (idx = 0; ; idx++) {
        const char *obj_start = strchr(p, '{');
        if (!obj_start) return -1;
        p = obj_start + 1;
        if (idx == n) {
            memset(out, 0, sizeof(*out));
            extract_json_string_field(obj_start, "path", out->path, sizeof(out->path));
            out->shard_start = extract_json_uint64_field(obj_start, "shard_start");
            out->shard_count = extract_json_uint64_field(obj_start, "shard_count");
            out->size        = extract_json_uint64_field(obj_start, "size");
            /* shard_count defaults to 1 if absent (old format compat). */
            if (out->shard_count == 0) out->shard_count = 1;
            if (!out->path[0]) return -1;
            /* Same writable-roots/".." guard as build_manifest_index: the
             * spool apply path (runtime_apply_spool) fopen()s out->path
             * directly, so a manifest path outside the allowed roots would
             * otherwise be an arbitrary file write. */
            if (!is_path_allowed(out->path)) return -1;
            return 0;
        }
        /* Skip past this object's closing brace. Brace-aware (not strchr)
         * so a '}' inside an earlier entry's path can't mis-count which
         * object we're on. Manifest is NUL-terminated → NULL limit is fine. */
        {
            const char *obj_end = json_object_end(obj_start, NULL);
            if (!obj_end) return -1;
            p = obj_end + 1;
        }
    }
}

/*
 * Read the persisted manifest JSON into a heap-allocated buffer sized to
 * the on-disk file. Caller owns the returned pointer and must `free()` it.
 *
 * Returns 0 on success with `*out_buf` and `*out_len` populated, -1 on
 * file-missing/unreadable/oversize/oom (in which case `*out_buf` is NULL).
 *
 * `PS5UPLOAD2_MAX_MANIFEST_BLOB` (128 MiB) caps the read — a manifest
 * past that size is presumed corrupt or hostile.
 *
 * Why this exists: the spool fallback used to use a `char[8192]` stack
 * buffer with `runtime_read_manifest`, which silently truncated any
 * manifest larger than ~60 file entries — exactly the same bug 2.1
 * fixed in the direct-mode path with a heap-allocated `manifest_blob`.
 * Spool wasn't migrated; this function closes that gap.
 */
static int runtime_read_manifest_alloc(const runtime_tx_entry_t *entry,
                                        char **out_buf, size_t *out_len) {
    char path[512];
    FILE *fp = NULL;
    long fsize = 0;
    char *buf = NULL;
    size_t got = 0;
    if (!entry || !out_buf || !out_len) return -1;
    *out_buf = NULL;
    *out_len = 0;
    snprintf(path, sizeof(path), "%s/manifest_%s.json",
             PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "rb");
    if (!fp) return -1;
    if (fseek(fp, 0, SEEK_END) != 0) {
        fclose(fp);
        return -1;
    }
    fsize = ftell(fp);
    if (fsize <= 0 || (uint64_t)fsize > PS5UPLOAD2_MAX_MANIFEST_BLOB) {
        fprintf(stderr, "[payload2] manifest file size out of range: %ld bytes for tx %s\n",
                fsize, entry->tx_id_hex);
        fclose(fp);
        return -1;
    }
    rewind(fp);
    buf = (char *)malloc((size_t)fsize + 1);
    if (!buf) {
        fclose(fp);
        return -1;
    }
    got = fread(buf, 1, (size_t)fsize, fp);
    fclose(fp);
    if (got != (size_t)fsize) {
        free(buf);
        return -1;
    }
    buf[fsize] = '\0';
    *out_buf = buf;
    *out_len = (size_t)fsize;
    return 0;
}

/*
 * Persist the manifest JSON to disk during BEGIN_TX so apply can use it.
 */
static int runtime_write_manifest(const runtime_tx_entry_t *entry,
                                   const char *manifest_json,
                                   size_t manifest_len) {
    char path[512];
    FILE *fp = NULL;
    if (!entry || !manifest_json || manifest_len == 0) return -1;
    snprintf(path, sizeof(path), "%s/manifest_%s.json",
             PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "w");
    if (!fp) return -1;
    fwrite(manifest_json, 1, manifest_len, fp);
    fclose(fp);
    return 0;
}

/*
 * After a successful COMMIT_TX: apply spooled shards to the filesystem.
 *
 * Single-file (kind==1, !multi_file):
 *   Concatenate shards 1..shards_received into dest_root.
 *
 * Multi-file (kind==2, multi_file — may carry a single file):
 *   Read the persisted manifest; shard K maps to files[K-1].path.
 *   Each shard is a complete file (one shard per file).
 *
 * Parent directories are created as needed.
 */
/* Helper: throttled emission of an APPLY_PROGRESS frame.
 *
 * State (last_emit_us / last_emit_fi / bytes_running_total) is kept in
 * caller-owned variables so multiple invocations share rate-limiting.
 * Emits when:
 *   - last emit was >= 1 sec ago, OR
 *   - >= 1024 files have been applied since last emit, OR
 *   - `force` is set (used at the very start + end so the client sees
 *     motion immediately and a final "done" frame).
 *
 * On send failure (client disconnected mid-apply) we DO NOT propagate
 * the error — apply must continue regardless. The client's commit-wait
 * loop will see EOF on the connection and surface a real failure if
 * the disconnect was genuine, but we don't want a single missed
 * progress frame to abort an in-progress commit.
 *
 * Returns 0 if a frame was emitted (or skipped by throttling), -1 on
 * encoding error. Callers ignore the return value.
 */
static int runtime_emit_apply_progress(int client_fd,
                                       uint64_t trace_id,
                                       uint64_t files_applied,
                                       uint64_t total_files,
                                       uint64_t bytes_applied,
                                       uint64_t *last_emit_us,
                                       uint64_t *last_emit_fi,
                                       int force) {
    if (client_fd < 0) return 0;
    uint64_t now = now_us();
    uint64_t since_us = (last_emit_us && *last_emit_us > 0)
        ? (now - *last_emit_us) : (uint64_t)-1;
    uint64_t since_fi = (last_emit_fi)
        ? (files_applied - *last_emit_fi) : files_applied;
    /* Throttle: at most one frame per second or per 1024 files,
     * whichever first. Always emit on `force` (first and last). */
    if (!force && since_us < 1000000ULL && since_fi < 1024ULL) {
        return 0;
    }
    char body[160];
    int n = snprintf(body, sizeof(body),
        "{\"files_applied\":%llu,\"total_files\":%llu,\"bytes_applied\":%llu}",
        (unsigned long long)files_applied,
        (unsigned long long)total_files,
        (unsigned long long)bytes_applied);
    if (n <= 0 || (size_t)n >= sizeof(body)) return -1;
    /* Best-effort send. Ignore the return — see fn-level comment. */
    (void)send_frame(client_fd, FTX2_FRAME_APPLY_PROGRESS, 0, trace_id,
                     body, (size_t)n);
    if (last_emit_us) *last_emit_us = now;
    if (last_emit_fi) *last_emit_fi = files_applied;
    return 0;
}

static int runtime_apply_spool(const runtime_tx_entry_t *entry,
                               int client_fd,
                               uint64_t trace_id) {
    char spool_dir[512];
    char shard_path[512];
    char buf[65536];
    uint64_t seq = 0;
    /* P3 progress tracking: only meaningful when the client opted in
     * via FTX2_TX_FLAG_APPLY_PROGRESS_REQUESTED and we're in a multi-
     * file tx. last_emit_us/fi are written by runtime_emit_apply_progress.
     * bytes_running carries running totals for the body's bytes_applied
     * field; we increment as each file's manifest size is committed. */
    int progress_enabled = (entry && entry->apply_progress_enabled);
    uint64_t last_emit_us = 0;
    uint64_t last_emit_fi = 0;
    uint64_t bytes_running = 0;

    if (!entry || entry->dest_root[0] == '\0') return -1;

    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);

    /* ── Multi-file path ── (kind==2; may carry a single file) */
    if (entry->multi_file) {
        /* Heap-allocate the manifest sized to the on-disk file. The
         * direct-mode path was migrated to a heap blob in 2.1 to fix
         * silent corruption on >60-file transfers; this spool path
         * was missed. Fixed here: a single 8 KiB stack buffer would
         * truncate any real-game manifest mid-object and produce
         * "manifest missing entry N" errors at apply time, OR walk
         * into garbage. Caller owns the buffer; freed before returning. */
        char *manifest_buf = NULL;
        size_t manifest_len = 0;
        uint64_t fi = 0;
        int rc_apply = 0;
        if (runtime_read_manifest_alloc(entry, &manifest_buf, &manifest_len) != 0) {
            fprintf(stderr, "[payload2] could not read manifest for tx %s\n",
                    entry->tx_id_hex);
            return -1;
        }
        /* P3 emit-start: send a single APPLY_PROGRESS frame with
         * files_applied=0 so the client immediately sees motion (the
         * counter ticks from undefined → "0 of N"). Throttle state
         * threads through the loop. */
        if (progress_enabled) {
            runtime_emit_apply_progress(client_fd, trace_id,
                0, entry->file_count, 0,
                &last_emit_us, &last_emit_fi, 1 /* force */);
        }
        for (fi = 0; fi < entry->file_count; fi++) {
            manifest_file_entry_t mf;
            uint64_t s = 0;
            FILE *out = NULL;
            if (manifest_get_nth_file_path(manifest_buf, fi, &mf) != 0) {
                fprintf(stderr, "[payload2] manifest missing entry %llu\n",
                        (unsigned long long)fi);
                rc_apply = -1;
                break;
            }
            if (ensure_parent_dir(mf.path) != 0) {
                fprintf(stderr, "[payload2] ensure_parent_dir failed: %s\n", mf.path);
                rc_apply = -1;
                break;
            }
            out = fopen(mf.path, "wb");
            if (!out) {
                fprintf(stderr, "[payload2] open dest failed: %s errno=%d\n",
                        mf.path, errno);
                rc_apply = -1;
                break;
            }
            for (s = 0; s < mf.shard_count; s++) {
                uint64_t this_seq = mf.shard_start + s;
                char ibuf[65536];
                FILE *in = NULL;
                size_t got = 0;
                snprintf(shard_path, sizeof(shard_path), "%s/%llu",
                         spool_dir, (unsigned long long)this_seq);
                in = fopen(shard_path, "rb");
                if (!in) {
                    fprintf(stderr, "[payload2] open shard failed: %s errno=%d\n",
                            shard_path, errno);
                    rc_apply = -1;
                    break;
                }
                /* Stream this shard into `out`. On fwrite failure we
                 * mark rc_apply and break the while; the unconditional
                 * fclose(in) below closes `in` exactly once across
                 * both success and fwrite-fail paths. The pre-self-
                 * audit edit closed `in` inside the fwrite branch AND
                 * after the while, which was a double-close UB. */
                while ((got = fread(ibuf, 1, sizeof(ibuf), in)) > 0) {
                    if (fwrite(ibuf, 1, got, out) != got) {
                        rc_apply = -1;
                        break;
                    }
                }
                fclose(in);
                if (rc_apply != 0) break;
            }
            /* Close `out` exactly once iff it was successfully opened.
             * Three early-break paths above (manifest_get_nth_file_path
             * failure, ensure_parent_dir failure, fopen-out failure)
             * reach this point with `out == NULL` — `fclose(NULL)` is
             * undefined behaviour in C and on PS5 libc typically
             * crashes. The two break paths inside for(s) (open-shard
             * failure, fwrite failure) reach here with `out` already
             * opened, so the guarded close is correct.
             *
             * History: the previous edit pair (pre-self-audit + the
             * post-self-audit "single fclose" rewrite) both had this
             * gap — the rewrite traded a double-close on `in` for an
             * fclose-NULL on `out`. Resolved here with the explicit
             * non-null guard. */
            if (out) fclose(out);
            if (rc_apply != 0) break;
            /* Commit-time integrity (spool analogue of the direct-path
             * size checks in COMMIT_TX): the concatenated file must match
             * the manifest size. A missing/short spool shard — e.g. a
             * partial write left by a power-loss-interrupted run — would
             * otherwise publish a short file that reports success. Fail
             * loudly instead. (size 0 / absent: skip, nothing to verify.) */
            if (mf.size > 0) {
                struct stat st_sp;
                int ok = (stat(mf.path, &st_sp) == 0);
                if (!ok || (uint64_t)st_sp.st_size != mf.size) {
                    fprintf(stderr,
                            "[payload2] spool apply size mismatch %s: have %lld want %llu\n",
                            mf.path, (long long)(ok ? (long long)st_sp.st_size : -1),
                            (unsigned long long)mf.size);
                    rc_apply = -1;
                    break;
                }
            }
            printf("[payload2] applied %llu shards -> %s\n",
                   (unsigned long long)mf.shard_count, mf.path);
            /* P3 emit-tick: update running totals and emit a throttled
             * progress frame. The emit helper takes care of the rate
             * limit (>=1 sec OR >=1024 files since last emit). */
            if (progress_enabled) {
                bytes_running += mf.size;
                runtime_emit_apply_progress(client_fd, trace_id,
                    fi + 1, entry->file_count, bytes_running,
                    &last_emit_us, &last_emit_fi, 0 /* throttle */);
            }
        }
        /* P3 emit-end: force a final progress frame at completion so
         * the client's counter lands at total/total before the
         * CommitTxAck arrives (rather than at total-1 or wherever the
         * last throttled tick fell). Skipped on error path — the
         * caller's apply_failed branch sends a structured Error frame
         * which the client treats as terminal. */
        if (progress_enabled && rc_apply == 0) {
            runtime_emit_apply_progress(client_fd, trace_id,
                entry->file_count, entry->file_count, bytes_running,
                &last_emit_us, &last_emit_fi, 1 /* force */);
        }
        free(manifest_buf);
        if (rc_apply == 0) {
            printf("[payload2] multi-file apply done: %llu files\n",
                   (unsigned long long)entry->file_count);
        }
        return rc_apply;
    }

    /* ── Single-file path: concatenate all shards to dest_root ── */
    {
        FILE *out = NULL;
        if (ensure_parent_dir(entry->dest_root) != 0) {
            fprintf(stderr, "[payload2] ensure_parent_dir failed: %s\n", entry->dest_root);
            return -1;
        }
        out = fopen(entry->dest_root, "wb");
        if (!out) {
            fprintf(stderr, "[payload2] open dest_root failed: %s errno=%d\n",
                    entry->dest_root, errno);
            return -1;
        }
        for (seq = 1; seq <= entry->shards_received; seq++) {
            FILE *in = NULL;
            size_t got = 0;
            snprintf(shard_path, sizeof(shard_path), "%s/%llu",
                     spool_dir, (unsigned long long)seq);
            in = fopen(shard_path, "rb");
            if (!in) {
                fprintf(stderr, "[payload2] open shard failed: %s errno=%d\n",
                        shard_path, errno);
                fclose(out);
                return -1;
            }
            while ((got = fread(buf, 1, sizeof(buf), in)) > 0) {
                if (fwrite(buf, 1, got, out) != got) {
                    fclose(in);
                    fclose(out);
                    return -1;
                }
            }
            fclose(in);
        }
        fclose(out);
        /* Commit-time integrity: single-file spool result must match the
         * manifest's total size before we declare success. */
        if (entry->total_bytes > 0) {
            struct stat st_sp;
            int ok = (stat(entry->dest_root, &st_sp) == 0);
            if (!ok || (uint64_t)st_sp.st_size != entry->total_bytes) {
                fprintf(stderr,
                        "[payload2] spool apply size mismatch %s: have %lld want %llu\n",
                        entry->dest_root, (long long)(ok ? (long long)st_sp.st_size : -1),
                        (unsigned long long)entry->total_bytes);
                return -1;
            }
        }
        printf("[payload2] apply done: %llu shards -> %s\n",
               (unsigned long long)entry->shards_received, entry->dest_root);
        return 0;
    }
}

/*
 * Remove all shard files in the spool directory for a transaction and
 * then rmdir the directory itself. Called after a successful apply.
 */
static int runtime_cleanup_spool(const runtime_tx_entry_t *entry) {
    char spool_dir[512];
    DIR *dir = NULL;
    struct dirent *de = NULL;

    if (!entry) return -1;
    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);

    dir = opendir(spool_dir);
    if (!dir) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    while ((de = readdir(dir)) != NULL) {
        char path[512];
        if (de->d_name[0] == '.') continue;
        snprintf(path, sizeof(path), "%s/%s", spool_dir, de->d_name);
        (void)unlink(path);
    }
    closedir(dir);
    (void)rmdir(spool_dir);
    return 0;
}

/*
 * Resume integrity: recompute the durable cursor from what's actually on
 * disk instead of trusting the journaled shard COUNT.
 *
 * The journal can run AHEAD of the bytes that truly reached storage:
 * streaming shard writes and the journal flush aren't fsync'd, so a
 * power-loss / rest-mode kill can lose page-cache data (worst on
 * USB/exFAT). Trusting `shards_received` as the resume point would then
 * make the client SKIP shards whose data never landed → a file silently
 * missing interior bytes (the "ps5upload corrupts, FTP is fine" report).
 *
 * Reconcile at FILE granularity, which is the safe unit: each file's
 * first shard truncates its tmp, so re-sending a whole file rewrites it
 * cleanly — no partial-shard append-offset hazard. A file is "durable"
 * if EITHER its final path OR its `<path>.ps5up2-tmp` exists at exactly
 * the manifest size (packed small files land at the final path,
 * non-packed at the tmp). Clamp `shards_received` down to
 * (first non-durable file's shard_start - 1) so the client re-sends from
 * there. This only ever LOWERS the cursor; a too-low result merely
 * re-sends (idempotent), and the COMMIT_TX size check is the backstop.
 */
static void reconcile_resume_cursor(runtime_tx_entry_t *entry) {
    const manifest_index_entry_t *idx;
    uint64_t i;
    uint64_t min_bad_start = 0; /* 0 = no gap found */
    if (!entry || !entry->manifest_index || entry->manifest_index_count == 0) {
        return;
    }
    if (!entry->manifest_blob) return;
    idx = (const manifest_index_entry_t *)entry->manifest_index;
    for (i = 0; i < entry->manifest_index_count; i++) {
        char path[512];
        char tmp_path[512 + 16];
        size_t plen = idx[i].path_len;
        struct stat st;
        int durable = 0;
        if (idx[i].size == 0) continue; /* empty files: nothing to verify */
        if (plen == 0 || plen >= sizeof(path)) continue;
        if ((uint64_t)idx[i].path_offset + (uint64_t)plen > entry->manifest_blob_len) {
            continue;
        }
        if (json_copy_unescaped_string(entry->manifest_blob + idx[i].path_offset,
                                       entry->manifest_blob + idx[i].path_offset + plen,
                                       path, sizeof(path)) != 0) {
            continue;
        }
        if (stat(path, &st) == 0 && (uint64_t)st.st_size == idx[i].size) {
            durable = 1;
        } else {
            snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", path);
            if (stat(tmp_path, &st) == 0 && (uint64_t)st.st_size == idx[i].size) {
                durable = 1;
            }
        }
        if (!durable &&
            (min_bad_start == 0 || idx[i].shard_start < min_bad_start)) {
            min_bad_start = idx[i].shard_start;
        }
    }
    if (min_bad_start > 0) {
        uint64_t reconciled = min_bad_start - 1;
        if (reconciled < entry->shards_received) {
            fprintf(stderr,
                    "[payload2] resume reconcile tx %s: cursor %llu -> %llu "
                    "(first non-durable file starts at shard %llu)\n",
                    entry->tx_id_hex,
                    (unsigned long long)entry->shards_received,
                    (unsigned long long)reconciled,
                    (unsigned long long)min_bad_start);
            entry->shards_received = reconciled;
        }
    }
}

/* Encode a SHARD_ACK into a 48-byte buffer (no struct padding assumptions). */
static void encode_shard_ack(unsigned char *out,
                              const unsigned char *tx_id,
                              uint64_t shard_seq,
                              uint8_t ack_state,
                              uint64_t bytes_committed,
                              uint64_t files_committed) {
    memset(out, 0, FTX2_SHARD_ACK_LEN);
    memcpy(out, tx_id, 16);
    write_le64(out + 16, shard_seq);
    out[24] = ack_state;
    /* out[25..31] = pad (zeroed) */
    write_le64(out + 32, bytes_committed);
    write_le64(out + 40, files_committed);
}

/*
 * Packed-shard handler.
 *
 * Body layout (set of `record_count` back-to-back records):
 *   [u32 path_len LE][u32 data_len LE][path bytes][data bytes]
 *
 * Each record is written straight to `<path>.ps5up2-tmp` so the COMMIT_TX
 * rename loop picks it up just like the single-file-per-shard path. No
 * manifest_index lookup is involved — packed shards are self-describing.
 *
 * BLAKE3 digest in ShardHeader covers the entire body (record prefixes +
 * paths + data in wire order). We stream-hash on every byte received.
 *
 * On any parse / write / digest error the transaction is aborted via
 * `runtime_abort_tx_fatal`. There is no per-record retry — packed writes
 * are append-atomic only within a record; a partial pack corrupts tmps.
 */
static int handle_packed_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id,
                                runtime_tx_entry_t *entry,
                                const unsigned char *tx_id,
                                uint64_t shard_seq,
                                uint64_t body_len,
                                uint32_t record_count,
                                const unsigned char *expected_digest,
                                int want_verify) {
    blake3_hasher hasher;
    uint64_t remaining = body_len;
    uint32_t r;
    unsigned char ack_bytes[FTX2_SHARD_ACK_LEN];
    /* Lazily start the per-tx worker pool on first packed shard. If the
     * start fails (OOM, kernel thread cap), fall back to serial inline
     * processing — functionally identical to pre-pool behaviour. */
    int pool_active = 0;
    if (entry && !entry->pack_pool) {
        if (pack_pool_start(entry) == 0) {
            pool_active = 1;
        } else {
            fprintf(stderr,
                    "[payload2] pack: worker pool start failed — serial fallback\n");
        }
    } else if (entry && entry->pack_pool) {
        pool_active = 1;
    }

    if (want_verify) blake3_hasher_init(&hasher);

    for (r = 0; r < record_count; r++) {
        unsigned char prefix[FTX2_PACKED_RECORD_PREFIX_LEN];
        uint32_t path_len;
        uint32_t rec_data_len;
        char path[512];

        if (remaining < FTX2_PACKED_RECORD_PREFIX_LEN) {
            if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "packed_truncated", 16);
        }
        if (recv_exact(client_fd, prefix, FTX2_PACKED_RECORD_PREFIX_LEN) != 0) return -1;
        if (want_verify) {
            blake3_hasher_update(&hasher, prefix, FTX2_PACKED_RECORD_PREFIX_LEN);
        }
        remaining -= FTX2_PACKED_RECORD_PREFIX_LEN;

        path_len     = read_le32(prefix);
        rec_data_len = read_le32(prefix + 4);

        if (path_len == 0 || path_len >= sizeof(path) ||
            (uint64_t)path_len + (uint64_t)rec_data_len > remaining) {
            if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "packed_bad_record", 17);
        }

        if (recv_exact(client_fd, path, path_len) != 0) return -1;
        if (want_verify) blake3_hasher_update(&hasher, (unsigned char *)path, path_len);
        path[path_len] = '\0';
        remaining -= path_len;

        /* Defence in depth: every per-record `path` carried by a packed
         * STREAM_SHARD must pass the same writable-roots allowlist used
         * elsewhere. Without this, a hostile LAN client could include
         * a record like "/system_data/priv/foo" + arbitrary bytes and
         * the open(O_WRONLY|O_CREAT|O_TRUNC) below would write it.
         * The BEGIN_TX dest_root check above is the first line of
         * defence; this is the second, since per-record paths are
         * sent independently and aren't constrained by the manifest
         * dest_root in any structural way. */
        if (!is_path_allowed(path)) {
            if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "packed_path_not_allowed", 23);
        }

        /* Parent-dir cache is reader-thread-only; this stays correct whether
         * the pool is active or not. */
        (void)ensure_parent_dir_cached(entry, path);

        if (pool_active) {
            /* Pool path: reader receives data into a heap buffer, pushes to
             * workers, hashes as it reads. Workers do open+write+close.
             *
             * Allocation pattern:
             *   path_buf  — malloc'd strdup, freed by worker
             *   data_buf  — malloc'd, may be NULL for 0-byte records,
             *               freed by worker
             */
            char          *path_buf = NULL;
            unsigned char *data_buf = NULL;

            path_buf = (char *)malloc((size_t)path_len + 1);
            if (!path_buf) {
                fprintf(stderr, "[payload2] pack: path alloc failed\n");
                if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "out_of_memory", 13);
            }
            memcpy(path_buf, path, (size_t)path_len + 1);

            if (rec_data_len > 0) {
                data_buf = (unsigned char *)malloc(rec_data_len);
                if (!data_buf) {
                    free(path_buf);
                    fprintf(stderr, "[payload2] pack: data alloc %u failed\n", rec_data_len);
                    if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
                    runtime_abort_tx_fatal(state, entry);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                      "out_of_memory", 13);
                }
                if (recv_exact(client_fd, data_buf, rec_data_len) != 0) {
                    free(path_buf);
                    free(data_buf);
                    return -1;
                }
                if (want_verify) blake3_hasher_update(&hasher, data_buf, rec_data_len);
            }

            if (pack_pool_push((pack_worker_pool_t *)entry->pack_pool,
                               path_buf, data_buf, rec_data_len) != 0) {
                /* Sticky worker error — push returned -1 without taking
                 * ownership, so we own the frees. Drain remaining bytes
                 * and bail. */
                free(path_buf);
                if (data_buf) free(data_buf);
                if (remaining > (uint64_t)rec_data_len) {
                    (void)drain_shard_data(client_fd, remaining - (uint64_t)rec_data_len);
                }
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "pack_worker_io_error", 20);
            }
            remaining -= rec_data_len;
            if (entry) entry->bytes_received += rec_data_len;
        } else {
            /* Serial fallback path: unchanged semantics from pre-pool code. */
            char tmp_path[512 + 16];
            int fd;
            snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", path);
            (void)unlink(tmp_path);

            fd = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, 0777);
            if (fd >= 0) (void)fchmod(fd, 0777);
            if (fd < 0) {
                (void)drain_shard_data(client_fd, (uint64_t)rec_data_len + remaining - (uint64_t)rec_data_len);
                remaining = 0;
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "packed_open_failed", 18);
            }
            if (rec_data_len > 0) {
                (void)ftruncate(fd, (off_t)rec_data_len);
            }
            {
                uint64_t rem = rec_data_len;
                unsigned char buf[FTX2_SHARD_DRAIN_BUF];
                while (rem > 0) {
                    size_t take = rem > sizeof(buf) ? sizeof(buf) : (size_t)rem;
                    if (recv_exact(client_fd, buf, take) != 0) { close(fd); return -1; }
                    if (want_verify) blake3_hasher_update(&hasher, buf, take);
                    if (write_full(fd, buf, take) != 0) {
                        /* Disk-full / I/O error writing a packed record. Like
                         * the pool path above, abort + report instead of a
                         * bare close (which the app shows as "PS5 crashed").
                         * JSON body so the engine's extract_payload_error
                         * lifts the errno into an actionable message. */
                        int we_p = errno;
                        char pwf[200];
                        int pwn = snprintf(pwf, sizeof(pwf),
                                           "{\"error\":\"fs_write_failed_errno_%d\","
                                           "\"detail\":\"packed write to destination "
                                           "failed: %s\"}",
                                           we_p, strerror(we_p));
                        close(fd);
                        runtime_abort_tx_fatal(state, entry);
                        if (pwn > 0) {
                            if ((size_t)pwn >= sizeof(pwf)) pwn = (int)sizeof(pwf) - 1;
                            return send_frame(client_fd, FTX2_FRAME_ERROR, 0,
                                              trace_id, pwf, (uint64_t)pwn);
                        }
                        return -1;
                    }
                    rem -= take;
                }
            }
            close(fd);
            remaining -= rec_data_len;
            if (entry) entry->bytes_received += rec_data_len;
        }
    }

    if (remaining > 0) {
        (void)drain_shard_data(client_fd, remaining);
        if (pool_active) (void)pack_pool_drain((pack_worker_pool_t *)entry->pack_pool);
        runtime_abort_tx_fatal(state, entry);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "packed_trailing_bytes", 21);
    }

    /* Drain the pool before ACK so SHARD_ACK means "persisted", preserving
     * the contract the serial path always had. */
    if (pool_active && pack_pool_drain((pack_worker_pool_t *)entry->pack_pool) != 0) {
        runtime_abort_tx_fatal(state, entry);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "pack_worker_io_error", 20);
    }

    if (want_verify) {
        unsigned char computed[BLAKE3_OUT_LEN];
        blake3_hasher_finalize(&hasher, computed, BLAKE3_OUT_LEN);
        if (memcmp(computed, expected_digest, BLAKE3_OUT_LEN) != 0) {
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "direct_tx_corrupt", 17);
        }
    }

    if (entry) {
        entry->shards_received += 1;
        runtime_append_shard_log(entry, shard_seq, body_len);
        /* Per-ACK journal flush enables shard-level resume (TX_FLAG_RESUME).
         * Without this, shards_received only hits disk at commit/abort, so
         * a network drop mid-transfer loses the counter and the reconnecting
         * client has no way to know what the payload already received. Cost
         * is one fopen+fprintf+fclose per shard ACK (~0.5 ms on PS5 UFS at
         * 32 MB shard size = <0.2% of shard-send time). */
        (void)runtime_flush_tx_record(state, entry);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    encode_shard_ack(ack_bytes, tx_id, shard_seq, FTX2_ACK_STATE_SPOOLED,
                     entry ? entry->bytes_received : 0, 0);
    return send_frame(client_fd, FTX2_FRAME_SHARD_ACK, 0, trace_id,
                      ack_bytes, FTX2_SHARD_ACK_LEN);
}

/*
 * Handle a STREAM_SHARD frame.
 *
 * Called BEFORE the main body-read path because shard bodies can be
 * very large (up to 32 MiB). We read the 64-byte shard header directly,
 * then drain the payload in chunks.
 *
 * Stub behaviour:
 *   - parse shard header
 *   - look up the transaction in the in-memory table
 *   - update per-tx shard stats
 *   - append to per-tx shard log
 *   - drain shard data (not yet written to storage)
 *   - reply with binary SHARD_ACK (spooled state)
 */
static int handle_stream_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, uint64_t body_len) {
    unsigned char shard_hdr_bytes[FTX2_SHARD_HEADER_LEN];
    unsigned char ack_bytes[FTX2_SHARD_ACK_LEN];
    uint64_t shard_seq = 0;
    uint64_t data_len = 0;
    unsigned char tx_id[16];
    runtime_tx_entry_t *entry = NULL;
    int rc = -1;

    if (!state) return -1;
    if (body_len < FTX2_SHARD_HEADER_LEN) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "shard_header_too_short", 22);
    }
    /* Wire-sanity cap, mirroring BEGIN_TX_BODY_MAX: the engine ships
     * 64 MiB shards (DEFAULT_SHARD_SIZE) — 256 MiB is 4× headroom for
     * any future bump while a malformed/hostile frame claiming a
     * multi-GiB body can no longer drive near-4 GiB malloc attempts in
     * the packed-record path or a multi-GiB socket drain. */
    if (body_len > (uint64_t)256 * 1024 * 1024) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "shard_body_too_large", 20);
    }
    if (recv_exact(client_fd, shard_hdr_bytes, FTX2_SHARD_HEADER_LEN) != 0) return -1;

    memcpy(tx_id, shard_hdr_bytes, 16);
    shard_seq = read_le64(shard_hdr_bytes + 16);

    /* Acquire the slot exclusively so a concurrent COMMIT/ABORT cannot
     * release entry->manifest_index / entry->direct_writer underneath
     * us mid-shard. NULL is fine — we still drain the body and emit a
     * not-found-shaped ACK like before, just without dereferencing
     * anything. Released via the `out:` cleanup. */
    entry = runtime_acquire_tx_entry(state, tx_id);

    /* shard_digest[24..56], record_count[56], flags[60] */
    {
        uint32_t record_count_hdr = read_le32(shard_hdr_bytes + 56);
        uint32_t flags_hdr        = read_le32(shard_hdr_bytes + 60);
        data_len = body_len - FTX2_SHARD_HEADER_LEN;

        if (flags_hdr & FTX2_SHARD_FLAG_PACKED) {
            int want_verify = 0;
            int i;
            for (i = 0; i < BLAKE3_OUT_LEN; i++) {
                if (shard_hdr_bytes[24 + i] != 0) { want_verify = 1; break; }
            }
            if (!entry || !entry->direct_mode || !entry->multi_file ||
                record_count_hdr == 0) {
                /* Packed shards are only meaningful for an active multi-file
                 * direct-mode tx (kind==2) with ≥1 records. Gate on
                 * `multi_file`, NOT `file_count > 1`: a kind==2 folder upload
                 * can legitimately carry a single (packed) small file — e.g.
                 * a resume that narrowed to one remaining file — and gating
                 * on the count rejected it as `packed_unsupported` (the
                 * Astro-Bot "many tiny files" report). Reject cleanly only
                 * when the tx genuinely isn't a multi-file direct receiver. */
                (void)drain_shard_data(client_fd, data_len);
                if (entry) runtime_abort_tx_fatal(state, entry);
                rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                "packed_unsupported", 18);
                goto out;
            }
            rc = handle_packed_shard(state, client_fd, trace_id,
                                     entry, tx_id, shard_seq,
                                     data_len, record_count_hdr,
                                     shard_hdr_bytes + 24, want_verify);
            goto out;
        }
    }
    {
        unsigned char computed[BLAKE3_OUT_LEN];
        int want_verify = 0;
        int i = 0;
        /* Multi-file per-file durable promote (2.25.x). When this shard
         * completes its owning file in a kind==2 folder upload, the write
         * branch below records the file's tmp + final path here; the actual
         * rename is deferred until AFTER the per-shard digest check passes
         * (so a corrupt final shard never gets promoted). See the promote
         * block past the verify gate. */
        int mf_completed = 0;
        char mf_done_tmp[sizeof(((manifest_file_entry_t *)0)->path) + 16];
        char mf_done_final[sizeof(((manifest_file_entry_t *)0)->path)];
        mf_done_tmp[0] = '\0';
        mf_done_final[0] = '\0';
        /* Only hash if the sender provided a non-zero expected digest. */
        for (i = 0; i < BLAKE3_OUT_LEN; i++) {
            if (shard_hdr_bytes[24 + i] != 0) { want_verify = 1; break; }
        }
        if (data_len > 0) {
            int write_ok;
            if (entry && entry->direct_mode && !entry->multi_file) {
                /* Single-file direct (kind==1): stream into <dest>.ps5up2-tmp
                 * via a persistent fd and (for large txs) a persistent writer
                 * thread. First shard opens + ftruncates, COMMIT_TX closes +
                 * renames, and any terminal transition (ABORT / takeover /
                 * shutdown / fatal) unlinks the tmp through
                 * runtime_release_tx_resources. */
                int is_first = (shard_seq == 1);
                uint64_t prealloc = (is_first && entry->total_bytes > 0)
                                      ? entry->total_bytes : 0;
                write_ok = runtime_write_shard_persistent(entry, client_fd,
                                                          data_len,
                                                          is_first, prealloc,
                                                          want_verify ? computed : NULL);
            } else if (entry && entry->direct_mode && entry->multi_file) {
                /* Multi-file direct: route this shard to the owning file's
                 * tmp via the in-memory manifest index built at BEGIN_TX.
                 * If the index is missing (e.g. a recovered-after-takeover
                 * tx whose client is gone), or routing fails, the shard
                 * is unroutable — we must NOT fall through to digest
                 * comparison with an uninitialised `computed` buffer. */
                manifest_file_entry_t mf;
                char tmp_path[sizeof(mf.path) + 16];
                if (!entry->manifest_index ||
                    lookup_manifest_index(
                        (const manifest_index_entry_t *)entry->manifest_index,
                        entry->manifest_index_count,
                        entry->manifest_blob,
                        entry->manifest_blob_len,
                        shard_seq, &mf) != 0) {
                    fprintf(stderr, "[payload2] direct multi: no manifest file owns shard %llu\n",
                            (unsigned long long)shard_seq);
                    (void)drain_shard_data(client_fd, data_len);
                    runtime_abort_tx_fatal(state, entry);
                    rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                    "manifest_shard_not_owned", 24);
                    goto out;
                }
                snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", mf.path);
                if (shard_seq == mf.shard_start) {
                    if (ensure_parent_dir_cached(entry, mf.path) != 0) {
                        fprintf(stderr, "[payload2] direct multi: parent dir failed %s\n",
                                mf.path);
                    }
                    (void)unlink(tmp_path);
                }
                /* Locate the MUTABLE index entry for this file so we can read +
                 * advance its bytes_written cursor. Binary search by shard_start
                 * (the value lookup_manifest_index just told us). Falls back to
                 * NULL on the impossible case of a slot mismatch, in which case
                 * we degrade gracefully to a no-prealloc, naive offset-from-0
                 * write — correct for a single-shard file. */
                manifest_index_entry_t *mut_idx_arr =
                    (manifest_index_entry_t *)entry->manifest_index;
                manifest_index_entry_t *idx_mut = NULL;
                {
                    uint64_t lo = 0, hi = entry->manifest_index_count;
                    while (lo < hi) {
                        uint64_t mid = lo + (hi - lo) / 2;
                        uint64_t s = mut_idx_arr[mid].shard_start;
                        if (s < mf.shard_start) lo = mid + 1;
                        else if (s > mf.shard_start) hi = mid;
                        else { idx_mut = &mut_idx_arr[mid]; break; }
                    }
                }
                /* New (2.16.0) write model: on the FIRST shard of every multi-
                 * file file — single-shard OR multi-shard — posix_fallocate the
                 * full file size up front so subsequent shards land in already-
                 * allocated blocks. The old code only preallocated single-shard
                 * files; multi-GB files (game images inside a folder dump) grew
                 * sparse-append, triggering UFS metadata churn → throughput
                 * collapse mid-transfer → engine 30 s socket-write timeout →
                 * connection drop while the serial accept loop is still busy =
                 * the screenshot's reconnect-refused failures. Subsequent
                 * shards lseek to the running `bytes_written` cursor instead
                 * of O_APPEND, since fallocate extends the file size on
                 * Prospero UFS and O_APPEND would land past the fallocated
                 * region. */
                {
                    int is_first = (shard_seq == mf.shard_start);
                    uint64_t prealloc = is_first ? mf.size : 0;
                    uint64_t write_offset = 0;
                    if (is_first) {
                        if (idx_mut) idx_mut->bytes_written = 0;
                    } else if (idx_mut) {
                        write_offset = idx_mut->bytes_written;
                    }
                    write_ok = runtime_write_shard_to_path(entry, tmp_path,
                                                           is_first, prealloc,
                                                           write_offset,
                                                           client_fd, data_len,
                                                           want_verify ? computed : NULL);
                    if (write_ok == 0 && idx_mut) {
                        idx_mut->bytes_written += data_len;
                        /* Did this shard finish the file? Gate on the genuine
                         * LAST shard of the file (shard_start + shard_count - 1)
                         * AND the full byte count — NOT bytes_written alone. A
                         * duplicated/retried shard re-adds data_len (the write
                         * itself is offset-correct + idempotent), so a byte-only
                         * check could cross mf.size on a non-last shard of a
                         * small file and promote a still-incomplete tmp. The
                         * shard_seq gate makes the trigger exact; the rename
                         * runs past the digest gate below (never promote a shard
                         * that fails verification). size==0 files ride inside
                         * packed shards, never here. */
                        uint64_t mf_last_shard =
                            mf.shard_start + mf.shard_count - 1;
                        if (mf.size > 0 && mf.shard_count > 0 &&
                            shard_seq == mf_last_shard &&
                            idx_mut->bytes_written >= mf.size) {
                            mf_completed = 1;
                            snprintf(mf_done_tmp, sizeof(mf_done_tmp),
                                     "%s.ps5up2-tmp", mf.path);
                            snprintf(mf_done_final, sizeof(mf_done_final),
                                     "%s", mf.path);
                        }
                    }
                }
            } else if (entry) {
                write_ok = runtime_write_shard_data(entry, shard_seq, client_fd,
                                                    data_len,
                                                    want_verify ? computed : NULL);
            } else {
                write_ok = drain_shard_data(client_fd, data_len);
            }
            if (write_ok != 0) {
                /* A direct-write helper failed (disk full / open / alloc /
                 * RAM pressure) and stashed the cause in
                 * entry->last_io_errno. Send a JSON ERROR frame — the shape
                 * the engine's extract_payload_error parses — so the host
                 * shows an actionable reason (ENOSPC -> "destination out of
                 * space") instead of a bare EOF, which the desktop app
                 * otherwise renders as the misleading "your PS5 stopped
                 * responding / crashed". The tx stays active so a later
                 * Retry can resume once space is freed. (No entry: nothing
                 * to report — keep the old silent close.) */
                if (entry) {
                    char wfr[200];
                    int we = entry->last_io_errno;
                    int wn;
                    if (we > 0) {
                        wn = snprintf(wfr, sizeof(wfr),
                                      "{\"error\":\"fs_write_failed_errno_%d\","
                                      "\"detail\":\"shard %llu write to "
                                      "destination failed: %s\"}",
                                      we, (unsigned long long)shard_seq,
                                      strerror(we));
                    } else {
                        wn = snprintf(wfr, sizeof(wfr),
                                      "{\"error\":\"fs_write_failed\","
                                      "\"detail\":\"shard %llu write to "
                                      "destination failed\"}",
                                      (unsigned long long)shard_seq);
                    }
                    if (wn > 0) {
                        if ((size_t)wn >= sizeof(wfr)) wn = (int)sizeof(wfr) - 1;
                        (void)send_frame(client_fd, FTX2_FRAME_ERROR, 0,
                                         trace_id, wfr, (uint64_t)wn);
                    }
                }
                rc = -1;
                goto out;
            }
        } else {
            /* Zero-length shard. */
            if (want_verify) {
                /* digest is BLAKE3 of empty input. */
                blake3_hasher h;
                blake3_hasher_init(&h);
                blake3_hasher_finalize(&h, computed, BLAKE3_OUT_LEN);
            }
            /* A 0-byte single-file direct upload still needs its
             * <dest>.ps5up2-tmp created so COMMIT_TX's temp->final rename
             * succeeds. The write dispatch above is gated on data_len>0, so
             * without this an empty single file fails commit with
             * `direct_rename_failed` (confirmed on hardware). Calling the
             * persistent writer with data_len=0 opens the tmp (O_CREAT|
             * O_TRUNC) and writes nothing — leaving the empty file for
             * COMMIT to close + rename, exactly like any 1-shard file.
             * Multi-file empty files don't need this: they ride inside
             * packed shards, which always have data_len>0. */
            if (entry && entry->direct_mode && !entry->multi_file &&
                shard_seq == 1 && !entry->direct_fd_open) {
                if (runtime_write_shard_persistent(entry, client_fd, 0, 1, 0,
                                                   NULL) != 0) {
                    rc = -1;
                    goto out;
                }
            }
        }
        /* Streaming verify — digest was computed while writing. */
        if (entry && want_verify &&
            memcmp(computed, shard_hdr_bytes + 24, BLAKE3_OUT_LEN) != 0) {
            if (entry->direct_mode) {
                /* Direct-write cannot surgically undo an already-appended
                 * suffix in the tmp file, so a mid-transfer digest mismatch
                 * is fatal for the whole transaction. Drop the tmp, mark the
                 * entry aborted, and return a distinct error so the host
                 * treats this as a hard failure (not a per-shard retry). */
                (void)unlink(entry->tmp_path);
                runtime_abort_tx_fatal(state, entry);
                rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                "direct_tx_corrupt", 17);
                goto out;
            }
            {
                char bad_path[512];
                snprintf(bad_path, sizeof(bad_path), "%s/spool_%s/%llu",
                         PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex,
                         (unsigned long long)shard_seq);
                (void)unlink(bad_path);
            }
            rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                            "shard_digest_mismatch", 21);
            goto out;
        }

        /* Per-file durable promote (2.25.x). A multi-file folder upload is one
         * BeginTx…CommitTx, and historically every file's `<path>.ps5up2-tmp`
         * was renamed to its final path ONLY at COMMIT_TX. So a payload crash
         * mid-folder (the 220 GB-game report: "payload no longer loaded" ~¼–½
         * through) lost EVERY streamed-but-uncommitted file — the next reconcile
         * scans only final dest paths, so it never saw the orphaned tmps and
         * re-sent the whole game, pinning "already present" at the same count
         * forever. Promote each file to its final path the instant its last
         * shard is verified, so a crash leaves completed files durable and the
         * next resume is naturally incremental (reconcile sees them, skips them).
         *
         * Reaching here means the digest gate above passed (or verify was off),
         * so the file is whole and correct. COMMIT_TX stays the backstop: it
         * finds the tmp gone + the file already at its final size and takes the
         * ENOENT==success path (the same one packed files already use). This is
         * a metadata-only rename — NO inline fsync: flushing a multi-GB file
         * here would stall the socket read long enough to trip the engine's
         * write timeout (the very drop we fight elsewhere), and process-crash
         * recovery only needs the bytes in the kernel page cache, which a
         * rename preserves. Power-loss durability is unchanged from the old
         * commit-time rename (both rely on un-fsync'd data) and is covered by
         * Safe-mode hashing. A rename failure is non-fatal: leave the tmp and
         * let COMMIT_TX promote it the old way. */
        if (entry && mf_completed) {
            if (rename(mf_done_tmp, mf_done_final) != 0 && errno != ENOENT) {
                fprintf(stderr,
                        "[payload2] per-file promote %s -> %s errno=%d "
                        "(deferring to commit)\n",
                        mf_done_tmp, mf_done_final, errno);
            }
        }
    }

    /* Update per-tx stats. */
    if (entry) {
        entry->shards_received += 1;
        entry->bytes_received  += data_len;
        runtime_append_shard_log(entry, shard_seq, data_len);
        /* Per-ACK journal flush enables shard-level resume — see the matching
         * call in handle_packed_stream_shard for rationale. */
        (void)runtime_flush_tx_record(state, entry);
    }

    /* Per-shard append to the tx event log removed: it costs ~0.5–1 ms/shard
     * in open/write/close and offered little diagnostic value beyond the
     * per-tx shard log (which is still written). begin/commit/abort/shutdown
     * events remain in the event log. */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    encode_shard_ack(ack_bytes,
                     tx_id,
                     shard_seq,
                     FTX2_ACK_STATE_SPOOLED,
                     entry ? entry->bytes_received : 0,
                     0);
    rc = send_frame(client_fd, FTX2_FRAME_SHARD_ACK, 0, trace_id,
                    ack_bytes, FTX2_SHARD_ACK_LEN);
out:
    if (entry) runtime_release_tx_entry(state, entry);
    return rc;
}

/* ── CLEANUP handler ──────────────────────────────────────────────────────────
 *
 * Exposes a narrow `rm -rf <path>` primitive to the host so bench/smoke
 * sweeps can reset PS5 state between profiles without waiting for a reboot.
 *
 * Safety:
 *   - Path must start with one of the allowlisted prefixes below. Anything
 *     else — `/data`, `/system`, `/`, empty, or paths containing `..` — is
 *     refused outright. This is not a general-purpose delete RPC.
 *   - Recursive delete stops on any removal error rather than pushing on
 *     through; the ACK reports how many files/dirs were removed.
 */

/* Unified test sandbox: everything the bench / smoke / sweep harnesses
 * write lives under `<root>/ps5upload/tests/…`, where `<root>` is one of:
 *
 *   /data                      built-in storage
 *   /mnt/ext[0-9]+             M.2 expansion slot (when mounted)
 *   /mnt/usb[0-9]+             USB storage slot (when mounted)
 *
 * Consolidating everything under this shape lets the user wipe the entire
 * test footprint on a drive with a single cleanup call — e.g.
 *   POST /api/ps5/cleanup {"path":"/data/ps5upload/tests"}
 *
 * Safety: the path is matched against the allowed shapes by the
 * `cleanup_path_allowed` helper below; literally everything else is
 * refused, so `/data/ps5upload/runtime`, `/data/ps5upload/tx`, and any
 * non-tests area stay off-limits.
 */
static int path_has_test_suffix(const char *p) {
    /* Matches ".../ps5upload/tests" optionally followed by '/<sub>'. */
    const char *suffix = "/ps5upload/tests";
    size_t slen = strlen(suffix);
    if (strncmp(p, suffix, slen) != 0) return 0;
    return (p[slen] == '\0' || p[slen] == '/');
}

static int cleanup_path_allowed(const char *path) {
    if (!path || !*path) return 0;
    /* Defence-in-depth: reject any `..` or `.` path component.
     * Component-scoped (matches is_path_allowed's semantics) so
     * legitimate test-folder names like `My..Tests` aren't rejected. */
    if (path_has_dotdot_component(path)) return 0;

    /* Case A: /data/ps5upload/tests[/...] */
    {
        const char *prefix = "/data";
        size_t plen = strlen(prefix);
        if (strncmp(path, prefix, plen) == 0 && path_has_test_suffix(path + plen)) {
            return 1;
        }
    }

    /* Case B: /mnt/{ext,usb}<digits>/ps5upload/tests[/...] */
    if (strncmp(path, "/mnt/", 5) == 0) {
        const char *p = path + 5;
        int is_ext = (strncmp(p, "ext", 3) == 0);
        int is_usb = (strncmp(p, "usb", 3) == 0);
        if (is_ext || is_usb) {
            p += 3;
            /* One or more digits after ext/usb. */
            if (*p < '0' || *p > '9') return 0;
            while (*p >= '0' && *p <= '9') p++;
            if (path_has_test_suffix(p)) return 1;
        }
    }

    return 0;
}

/* Recursive removal of a single path. Counts files + dirs actually removed
 * so the ACK can report work done. Returns 0 on success, -1 on error. A
 * missing path (ENOENT) counts as success with zero removals.
 *
 * `depth` bounds the recursion to 64 levels — matches rm_rf_op /
 * chmod_rf so a pathological symlink loop or hostile cleanup target
 * can't blow the stack. Pre-2.2.28 was unbounded; the path is bench-
 * test-only today so the risk was theoretical, but the inconsistency
 * with the other recursive helpers was a footgun. */
#define REMOVE_RECURSIVE_MAX_DEPTH 64
static int remove_recursive_path_inner(const char *path,
                                        uint64_t *removed_files,
                                        uint64_t *removed_dirs,
                                        int depth) {
    struct stat st;
    DIR *dir = NULL;
    struct dirent *ent = NULL;
    if (!path) return -1;
    if (depth > REMOVE_RECURSIVE_MAX_DEPTH) {
        fprintf(stderr,
                "[payload2] remove_recursive: depth cap %d hit at %s\n",
                REMOVE_RECURSIVE_MAX_DEPTH, path);
        return -1;
    }
    if (lstat(path, &st) != 0) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        if (unlink(path) != 0 && errno != ENOENT) return -1;
        if (removed_files) *removed_files += 1;
        return 0;
    }
    dir = opendir(path);
    if (!dir) return -1;
    while ((ent = readdir(dir)) != NULL) {
        char child[512];
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        if (snprintf(child, sizeof(child), "%s/%s", path, ent->d_name) >= (int)sizeof(child)) {
            closedir(dir);
            return -1; /* path too long — refuse to truncate and leak state */
        }
        if (remove_recursive_path_inner(child, removed_files, removed_dirs,
                                        depth + 1) != 0) {
            closedir(dir);
            return -1;
        }
    }
    closedir(dir);
    if (rmdir(path) != 0 && errno != ENOENT) return -1;
    if (removed_dirs) *removed_dirs += 1;
    return 0;
}

static int remove_recursive_path(const char *path,
                                  uint64_t *removed_files,
                                  uint64_t *removed_dirs) {
    return remove_recursive_path_inner(path, removed_files, removed_dirs, 0);
}

static int handle_cleanup(runtime_state_t *state, int client_fd,
                           uint64_t trace_id,
                           const char *request_body, uint64_t body_len) {
    char path[512];
    char resp[256];
    uint64_t removed_files = 0;
    uint64_t removed_dirs = 0;
    int rc;
    int len;
    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
    }
    if (!path[0]) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_missing_path", 20);
    }
    if (!cleanup_path_allowed(path)) {
        fprintf(stderr, "[payload2] cleanup: refusing disallowed path %s\n", path);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_path_denied", 19);
    }
    rc = remove_recursive_path(path, &removed_files, &removed_dirs);
    if (rc != 0) {
        fprintf(stderr, "[payload2] cleanup: remove_recursive_path(%s) failed errno=%d\n",
                path, errno);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_io_error", 16);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    {
        char path_esc[1024];
        json_escape_into(path, path_esc, sizeof(path_esc));
        len = snprintf(resp, sizeof(resp),
                       "{\"ok\":true,\"path\":\"%s\",\"removed_files\":%llu,\"removed_dirs\":%llu}",
                       path_esc,
                       (unsigned long long)removed_files,
                       (unsigned long long)removed_dirs);
    }
    if (len < 0) return -1;
    return send_frame(client_fd, FTX2_FRAME_CLEANUP_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

/* ── FS_LIST_VOLUMES handler ─────────────────────────────────────────────────
 *
 * Enumerates storage volumes using `getmntinfo(MNT_WAIT)`, the FreeBSD
 * primitive that returns every mounted filesystem in one syscall. The old
 * per-path lstat+statfs probe has been retired: it had to guess at paths
 * (it missed the PS5-specific `/mnt/ext1` layout until we listed `/mnt`),
 * and it reported placeholder tmpfs slots as if they were real drives.
 *
 * Pattern matches the canonical example at
 * `/opt/ps5-payload-sdk/samples/mntinfo/main.c`.
 *
 * Each returned volume carries:
 *   path            — mount-on name (e.g. `/mnt/ext1`)
 *   mount_from      — device source (`/dev/nvme1`, `/dev/ssd0.user`, ...)
 *                     or pseudo name for tmpfs/etc.
 *   fs_type         — `bfs`, `nullfs`, `ufs`, `tmpfs`, `devfs`, ...
 *   total_bytes     — f_blocks × f_bsize
 *   free_bytes      — f_bavail × f_bsize (non-root availability)
 *   writable        — true if MNT_RDONLY is unset
 *   is_placeholder  — true for tmpfs/pseudo mounts or volumes <256 MiB.
 *                     UI filters these out by default; advanced views can
 *                     show them to make hot-plug state visible.
 *
 * Response size: a typical PS5 has ~40 mount entries (system partitions +
 * process sandboxes). At ~240 bytes/entry that's ~10 KiB; we heap-allocate
 * a 16 KiB buffer to keep the dispatch thread's stack bounded.
 */

/* Best-effort PS5 kernel version probe. Reads `kern.version` via
 * sysctl(2) — returns a string like "FreeBSD 11.0-RELEASE-pN #M ..."
 * that PS5 firmware extends with a sys-revision tag. Not the
 * user-visible firmware number (e.g. "5.00") — the PS5 doesn't
 * expose that to user-space via a stable sysctl we've found. The
 * kernel string is still useful: users can cross-reference it
 * against psdevwiki build strings to identify their console's
 * firmware exactly.
 *
 * Writes into `dst` (null-terminated). On failure writes "unknown"
 * and returns. Swallows errors — STATUS should never fail because
 * a sysctl was unavailable. */
static void read_ps5_kernel_version(char *dst, size_t dst_cap) {
    int mib[2] = { CTL_KERN, KERN_VERSION };
    size_t len = dst_cap;
    if (!dst || dst_cap == 0) return;
    if (sysctl(mib, 2, dst, &len, NULL, 0) != 0 || len == 0) {
        snprintf(dst, dst_cap, "unknown");
        return;
    }
    /* sysctl returns the length INCLUDING the trailing NUL most of the
     * time, but not always — clamp to dst_cap-1 and force-terminate
     * so downstream JSON embedding stays sane. Also replace any stray
     * newlines with spaces, since some builds embed them. */
    if (len > dst_cap - 1) len = dst_cap - 1;
    dst[len] = '\0';
    for (size_t i = 0; i < len; i++) {
        if (dst[i] == '\n' || dst[i] == '\r') dst[i] = ' ';
    }
}

/* JSON-escape path/device names. PS5 mount names are ASCII but defend
 * anyway — tamper-resistant for anything we feed into a JSON context.
 * Returns the number of bytes written (excluding the NUL). Callers
 * can detect truncation by comparing the return value against the
 * source strlen — if every source char was consumed, no truncation. */
static size_t json_escape_into(const char *src, char *dst, size_t dst_cap) {
    size_t ei = 0, ni = 0;
    if (!dst || dst_cap == 0) return 0;
    while (src && src[ni] && ei + 2 < dst_cap) {
        unsigned char c = (unsigned char)src[ni];
        if (c == '"' || c == '\\') {
            dst[ei++] = '\\';
            dst[ei++] = (char)c;
        } else if (c < 0x20) {
            dst[ei++] = '?';
        } else {
            dst[ei++] = (char)c;
        }
        ni++;
    }
    dst[ei] = '\0';
    return ei;
}

static const char *json_string_end(const char *start, const char *limit) {
    const char *p = start;
    if (!p) return NULL;
    while ((!limit || p < limit) && *p) {
        if (*p == '"') return p;
        if (*p == '\\') {
            p++;
            if ((limit && p >= limit) || !*p) return NULL;
        }
        p++;
    }
    return NULL;
}

static int json_copy_unescaped_string(const char *start, const char *end,
                                      char *out, size_t out_len) {
    size_t oi = 0;
    const char *p = start;
    if (!start || !end || !out || out_len == 0 || end < start) return -1;
    while (p < end) {
        unsigned char c = (unsigned char)*p++;
        if (c == '\\') {
            if (p >= end) return -1;
            c = (unsigned char)*p++;
            switch (c) {
                case '"': case '\\': case '/': break;
                case 'b': c = '\b'; break;
                case 'f': c = '\f'; break;
                case 'n': c = '\n'; break;
                case 'r': c = '\r'; break;
                case 't': c = '\t'; break;
                case 'u':
                    if (end - p < 4) return -1;
                    p += 4;
                    c = '?';
                    break;
                default:
                    return -1;
            }
        }
        if (c == '\0' || oi + 1 >= out_len) return -1;
        out[oi++] = (char)c;
    }
    out[oi] = '\0';
    return 0;
}

/*
 * Find the matching closing '}' of the JSON object beginning at `obj_start`
 * (which must point at the opening '{'), scanning past any '{' or '}' that
 * appears inside a quoted string value. Returns NULL if the object is
 * unterminated before `limit` (NULL `limit` scans to the NUL terminator).
 *
 * Why this exists: serde_json (the engine's serializer) emits '}' literally
 * inside string values, so a perfectly valid file path like
 * "/data/My}Game/eboot.bin" carries a brace that a naive strchr(obj_start,'}')
 * mistakes for the object terminator. That truncated the manifest entry
 * mid-parse and made BEGIN_TX reject the whole transfer with `manifest_invalid`
 * for any upload containing a '}' in a file or directory name. Tracking string
 * state (and backslash escapes, matching json_string_end) finds the real
 * object boundary regardless of what the path contains.
 */
static const char *json_object_end(const char *obj_start, const char *limit) {
    const char *p = obj_start;
    int in_string = 0;
    int depth = 0;
    if (!p || *p != '{') return NULL;
    while ((!limit || p < limit) && *p) {
        char c = *p;
        if (in_string) {
            if (c == '\\') {
                /* Skip the escaped char so an escaped quote (\") doesn't
                 * prematurely close the string. Mirrors json_string_end. */
                p++;
                if ((limit && p >= limit) || !*p) return NULL;
            } else if (c == '"') {
                in_string = 0;
            }
        } else if (c == '"') {
            in_string = 1;
        } else if (c == '{') {
            depth++;
        } else if (c == '}') {
            depth--;
            if (depth == 0) return p;
        }
        p++;
    }
    return NULL;
}

static const char *find_bounded(const char *hay, size_t hay_len,
                                const char *needle) {
    size_t needle_len = needle ? strlen(needle) : 0;
    if (!hay || !needle || needle_len == 0 || needle_len > hay_len) return NULL;
    for (size_t i = 0; i <= hay_len - needle_len; i++) {
        if (memcmp(hay + i, needle, needle_len) == 0) return hay + i;
    }
    return NULL;
}

/* Returns non-zero iff the mount is one we want to surface to the client:
 * the three PS5 storage shapes the UI cares about, and only when a real
 * device is backing the mount (not a sandbox nullfs view or a pseudo fs).
 *
 *   /data                    — internal SSD user partition
 *   /mnt/ext[0-9]+           — M.2 expansion *or* USB extended storage
 *                              (PS5 reformats both to UFS; they share
 *                              the `ext` namespace — `mount_from` is the
 *                              only way to tell them apart: `/dev/nvme*`
 *                              = M.2, `/dev/da*` = USB extended)
 *   /mnt/usb[0-9]+           — plain USB stick (exFAT / FAT32)
 *
 * Everything else — /user (nullfs alias of /data), /system*, /mnt/sandbox,
 * /mnt/pfs, /preinst, tmpfs, etc. — is hidden. We also drop entries whose
 * `mount_from` isn't under `/dev/` so an unmounted slot or a sandbox nullfs
 * with a /mnt/ext-shaped path can't sneak through. */
static int is_user_storage_path(const char *path) {
    if (!path) return 0;
    if (strcmp(path, "/data") == 0) return 1;
    if (strncmp(path, "/mnt/ext", 8) == 0) {
        /* guard against /mnt/externalsomething */
        return (path[8] >= '0' && path[8] <= '9') ? 1 : 0;
    }
    if (strncmp(path, "/mnt/usb", 8) == 0) {
        return (path[8] >= '0' && path[8] <= '9') ? 1 : 0;
    }
    /* Surface /mnt/ps5upload/<name>/ mounts — that's where FS_MOUNT
     * puts disk images, and users need to see them in the Volumes
     * list so they can trigger FS_UNMOUNT from the UI. The base
     * directory itself (/mnt/ps5upload without a child) is skipped. */
    if (strncmp(path, "/mnt/ps5upload/", 15) == 0 && path[15] != '\0') {
        return 1;
    }
    return 0;
}

static int handle_fs_list_volumes(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id) {
    const size_t RESP_CAP = 16u * 1024u;
    char *resp = NULL;
    size_t off = 0;
    struct statfs *mnts = NULL;
    int nmnts;
    int first_volume = 1;
    int i;
    int n;
    int rc;

    if (!state) return -1;

    /* Man page: buf is libc-owned; do NOT free. Each subsequent call
     * clobbers the previous buffer.
     *
     * MNT_NOWAIT (not MNT_WAIT) so we use the kernel's cached mount
     * table instead of forcing a fresh statfs on every mount. A fresh
     * statfs pass can block indefinitely if any mount is in an error
     * state (flaky USB, dead NAS, a .exfat image whose backing file
     * became unreachable). The cache is refreshed by the kernel on
     * actual mount/unmount events so it's always reasonably current
     * for a listing query -- and if it's slightly stale, the Volumes
     * tab just shows one spurious entry that disappears on the next
     * refresh, which is much better than a wedged mgmt thread. The
     * FS_UNMOUNT handler already uses MNT_NOWAIT for the same reason. */
    nmnts = mntinfo_snapshot(&mnts);
    if (nmnts < 0 || mnts == NULL) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_volumes_getmntinfo_failed", 33);
    }

    resp = (char *)malloc(RESP_CAP);
    if (!resp) {
        free(mnts);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_volumes_oom", 19);
    }

    n = snprintf(resp + off, RESP_CAP - off, "{\"volumes\":[");
    if (n < 0 || (size_t)n >= RESP_CAP - off) { free(resp); free(mnts); return -1; }
    off += (size_t)n;

    for (i = 0; i < nmnts; i++) {
        /* f_mntonname and f_mntfromname are fixed-size char arrays
         * inside struct statfs -- never NULL, but may be empty. Alias
         * them to local pointers so the filter conditions below read
         * cleanly without repeated mnts[i].* indexing. */
        const char *mnt_on   = mnts[i].f_mntonname;
        const char *mnt_from = mnts[i].f_mntfromname;
        uint64_t bs    = (uint64_t)mnts[i].f_bsize;
        uint64_t total = (uint64_t)mnts[i].f_blocks * bs;  /* full partition size */
        uint64_t bfree = (uint64_t)mnts[i].f_bfree  * bs;  /* free incl. root-only reserve */
        /* f_bavail (free usable by non-root) is signed on FreeBSD and can
         * read <=0 on a near-full FS; fall back to bfree so we never
         * publish a reserve bigger than the real free pool. */
        uint64_t avail = ((int64_t)mnts[i].f_bavail > 0)
                             ? (uint64_t)mnts[i].f_bavail * bs
                             : bfree;
        /* Reserve model: total is the FULL partition (f_blocks×f_bsize) and
         * the UFS root reserve (bfree-bavail) is left to fall into `used`
         * (used = total - avail). This matches the PS5 Settings → Storage
         * screen, which was confirmed against a live console to count the
         * reserve as used rather than shaving it off the headline total.
         * (An earlier build published total = raw - reserve here, which on a
         * 2 TB Pro understated the /data total by ~292 GB / 15% vs Settings.)
         * bfs/exfat report bavail≈bfree so their reserve is ~0 and this is a
         * no-op for USB/M.2 drives. */
        int writable   = (mnts[i].f_flags & MNT_RDONLY) ? 0 : 1;

        /* Surface decision in two halves:
         *
         *   1. "Ours" — anything we mounted, identified by either the
         *      legacy /mnt/ps5upload/<name> prefix OR a tracker file
         *      written by 2.2.25+ at a user-chosen path
         *      (e.g. /data/homebrew/PPSA17599). Surface unconditionally
         *      so the user can see the mount in Volumes and unmount it
         *      from the UI, regardless of whether it sits under one of
         *      the system-storage roots. Tracker check costs one stat()
         *      per mount; ~40 mounts per call so the FS_LIST_VOLUMES hot
         *      path still completes in well under a millisecond.
         *
         *   2. "System storage" — /data, /mnt/ext*, /mnt/usb*. Surfaced
         *      with the /dev/-prefix + total>0 filters that hide ghost
         *      hot-plug slots and LVD-layered mounts.
         *
         * Pre-2.2.51 the path-prefix allowlist gated the entire loop so
         * a user-chosen ffpkg mount at /data/homebrew/PPSA17599 was
         * filtered out before the tracker check ran — the mount
         * succeeded but the Volumes tab and Library mount-badge never
         * saw it, and the scanner only found games inside it via the
         * /data recursive walk (which can be cut off by the entry cap
         * on populated drives). */
        const int is_ours =
            (strncmp(mnt_on, "/mnt/ps5upload/", 15) == 0) ||
            mount_tracker_exists(mnt_on);
        if (!is_ours) {
            if (!is_user_storage_path(mnt_on)) continue;
            /* Real-device gate for the /mnt/ext* and /mnt/usb* slots:
             * their paths are hot-plug placeholders, so we require a
             * `/dev/` prefix on mount_from to avoid surfacing sandbox
             * nullfs mounts or unmounted slots. /data is exempt — it's
             * a single well-known internal mount whose backing can be
             * a label ref, a nullfs view on some firmware configurations,
             * or a block device. The path allowlist above already
             * guarantees /data is the real internal user partition, and
             * the total==0 check below filters ghost mounts regardless
             * of backing shape. */
            if (strcmp(mnt_on, "/data") != 0 &&
                strncmp(mnt_from, "/dev/", 5) != 0) continue;

            /* Sanity: a mount reporting zero blocks is either being set up
             * or tearing down — hide it rather than showing a broken slot. */
            if (total == 0) continue;
        }

        char path_esc[256];
        char from_esc[128];
        char source_esc[512] = "";
        json_escape_into(mnt_on,   path_esc, sizeof(path_esc));
        json_escape_into(mnt_from, from_esc, sizeof(from_esc));

        /* For our own mounts, look up the .src tracker to surface the
         * backing image path. Non-ours mounts leave source_image empty.
         * mount_tracker_read accepts the full mount_point and handles
         * both legacy /mnt/ps5upload/<name> and user-chosen paths. */
        if (is_ours) {
            char src_raw[256];
            if (mount_tracker_read(mnt_on, src_raw, sizeof(src_raw))) {
                json_escape_into(src_raw, source_esc, sizeof(source_esc));
            }
        }

        /* `is_placeholder` is kept in the schema for engine/UI compat
         * with older builds, but is always false now — the filter above
         * already rejects anything we'd have called a placeholder. */
        n = snprintf(resp + off, RESP_CAP - off,
                     "%s{\"path\":\"%s\",\"mount_from\":\"%s\","
                     "\"fs_type\":\"%s\","
                     "\"total_bytes\":%llu,\"free_bytes\":%llu,"
                     "\"writable\":%s,\"is_placeholder\":false,"
                     "\"source_image\":\"%s\"}",
                     first_volume ? "" : ",",
                     path_esc,
                     from_esc,
                     mnts[i].f_fstypename,
                     (unsigned long long)total,
                     (unsigned long long)avail,
                     writable ? "true" : "false",
                     source_esc);
        if (n < 0 || (size_t)n >= RESP_CAP - off) {
            fprintf(stderr, "[payload2] fs_list_volumes: response buffer full at %d/%d mounts\n",
                    i, nmnts);
            break;
        }
        off += (size_t)n;
        /* Defensive belt-and-braces clamp. The check above already
         * keeps us under RESP_CAP, but if it ever fails to (e.g. a
         * future code path adds an unchecked snprintf in the loop)
         * we'd underflow `RESP_CAP - off` in the next iteration into
         * a multi-GB size_t. Cap so the worst case is "nothing
         * appended" instead of "writes past the heap buffer". */
        if (off >= RESP_CAP) { off = RESP_CAP - 1; break; }
        first_volume = 0;
    }
    /* mnts isn't read past the loop; free the snapshot now so none of the
     * trailer/return paths below need to. */
    free(mnts);
    mnts = NULL;

    /* Reserve room for the "]}" trailer — if a previous iteration
     * landed on the boundary, leave space for the close-array tokens. */
    if (off + 2 >= RESP_CAP) { free(resp); return -1; }
    n = snprintf(resp + off, RESP_CAP - off, "]}");
    if (n < 0 || (size_t)n >= RESP_CAP - off) { free(resp); return -1; }
    off += (size_t)n;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_LIST_VOLUMES_ACK, 0, trace_id,
                    resp, (uint64_t)off);
    free(resp);
    return rc;
}

/* ── FS_LIST_DIR handler ─────────────────────────────────────────────────────
 *
 * Enumerates immediate children of a directory. Bounded response:
 *   - max 256 entries per call; clients paginate with `offset`
 *   - each entry is {name, kind, size}; kind in {file, dir, link, other}
 *   - response body sized to fit a 32 KiB stack buffer (256 × ~120B/entry)
 *
 * Safety: path must be absolute (`/`-prefixed) and must not contain `..`.
 * The payload does not impose a prefix allowlist here — read access is
 * gated by the kernel's own permissions on the running payload process.
 *
 * Request JSON: `{path, offset?, limit?}`
 * Response JSON: `{path, entries:[{name,kind,size}], truncated:bool, total_scanned:N}`
 */
static int handle_fs_list_dir(runtime_state_t *state, int client_fd,
                               uint64_t trace_id,
                               const char *request_body, uint64_t body_len) {
    const size_t RESP_CAP = 32u * 1024u;
    char path[512];
    char *resp = NULL;
    size_t off = 0;
    int n;
    int rc;
    DIR *dir = NULL;
    struct dirent *ent = NULL;
    uint64_t offset_req = 0;
    uint64_t limit_req = 256;
    uint64_t idx = 0;      /* running index over all readdir results */
    uint64_t emitted = 0;
    int first_entry = 1;
    int truncated = 0;

    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        {
            uint64_t v = extract_json_uint64_field(request_body, "offset");
            if (v > 0) offset_req = v;
        }
        {
            uint64_t v = extract_json_uint64_field(request_body, "limit");
            if (v > 0 && v < 1024) limit_req = v;
            /* Clamp to the buffer-safe ceiling. 256 ≈ 30 KiB response body. */
            if (limit_req > 256) limit_req = 256;
        }
    }
    /* 32 KiB on the stack is risky on PS5's bounded thread stacks; heap-alloc. */
    resp = (char *)malloc(RESP_CAP);
    if (!resp) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_oom", 15);
    }
    if (!path[0] || path[0] != '/') {
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_bad_path", 20);
    }
    /* Component-scoped traversal check: rejects `..` only when it
     * appears as a path component of its own. A bare `strstr("..")`
     * here used to false-positive on legitimate filenames like
     * `..some-cache` or `something..bak`, locking the user out of
     * their own directories via the File-System tab. The
     * destructive-op path already uses path_has_dotdot_component
     * for the same reason; FS_LIST_DIR was the lone outlier. */
    if (path_has_dotdot_component(path)) {
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_path_denied", 23);
    }

    dir = opendir(path);
    if (!dir) {
        char err[80];
        int el = snprintf(err, sizeof(err), "fs_list_dir_opendir_errno_%d", errno);
        if (el < 0) el = 0;
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id, err, (uint64_t)el);
    }

    {
        char path_esc[1024];
        json_escape_into(path, path_esc, sizeof(path_esc));
        n = snprintf(resp + off, RESP_CAP - off,
                     "{\"path\":\"%s\",\"entries\":[", path_esc);
    }
    if (n < 0 || (size_t)n >= RESP_CAP - off) {
        closedir(dir);
        free(resp);
        return -1;
    }
    off += (size_t)n;

    while ((ent = readdir(dir)) != NULL) {
        const char *name = ent->d_name;
        char full[1024];
        struct stat st;
        const char *kind = "other";
        uint64_t size = 0;
        int stat_ok;

        /* Skip "." and ".." — callers never care and they would otherwise
         * inflate every response by two entries. */
        if (name[0] == '.' && (name[1] == '\0' || (name[1] == '.' && name[2] == '\0'))) {
            continue;
        }

        if (idx < offset_req) {
            idx += 1;
            continue;
        }
        if (emitted >= limit_req) {
            truncated = 1;
            break;
        }

        if (snprintf(full, sizeof(full), "%s/%s", path, name) >= (int)sizeof(full)) {
            /* Skip without incrementing `idx`: the client uses
             * (entries.is_empty() || !truncated) to decide when
             * to stop paginating. If a page is dominated by
             * name-too-long entries, incrementing idx but emitting
             * nothing produced a 0-entry, !truncated response that
             * the client read as "directory exhausted" — making
             * everything past that page invisible. Leaving idx
             * untouched keeps the skipped entries invisible to
             * pagination so the loop continues and emits the
             * subsequent in-range entries on the next page. */
            continue;
        }
        stat_ok = (lstat(full, &st) == 0);
        if (stat_ok) {
            if (S_ISDIR(st.st_mode)) kind = "dir";
            else if (S_ISREG(st.st_mode)) { kind = "file"; size = (uint64_t)st.st_size; }
            else if (S_ISLNK(st.st_mode)) kind = "link";
        } else {
            /* stat failure is common on cross-mount symlinks and special
             * entries; skip silently rather than poisoning the whole list. */
            kind = "unknown";
        }

        /* Escape name for JSON: only double-quote and backslash need escaping;
         * control chars are rare in filenames and would require a bigger
         * escape table. Conservative: replace them with '?'. */
        {
            char esc[256];
            size_t ei = 0;
            size_t ni = 0;
            for (; name[ni] && ei + 2 < sizeof(esc); ni++) {
                unsigned char c = (unsigned char)name[ni];
                if (c == '"' || c == '\\') {
                    esc[ei++] = '\\';
                    esc[ei++] = (char)c;
                } else if (c < 0x20) {
                    esc[ei++] = '?';
                } else {
                    esc[ei++] = (char)c;
                }
            }
            esc[ei] = '\0';

            /* mtime in seconds since the Unix epoch, or 0 when stat
             * failed (matches the "unknown" kind branch). The
             * desktop's Library uses this to sort by recency
             * ("Most recent first") so a freshly-uploaded game
             * appears at the top without manual scrolling. */
            long long mtime_sec = stat_ok ? (long long)st.st_mtime : 0;
            n = snprintf(resp + off, RESP_CAP - off,
                         "%s{\"name\":\"%s\",\"kind\":\"%s\",\"size\":%llu,\"mtime\":%lld}",
                         first_entry ? "" : ",",
                         esc, kind,
                         (unsigned long long)size,
                         mtime_sec);
        }
        if (n < 0 || (size_t)n >= RESP_CAP - off) {
            truncated = 1;
            break;
        }
        off += (size_t)n;
        first_entry = 0;
        emitted += 1;
        idx += 1;
    }
    closedir(dir);

    n = snprintf(resp + off, RESP_CAP - off,
                 "],\"truncated\":%s,\"total_scanned\":%llu,\"returned\":%llu}",
                 truncated ? "true" : "false",
                 (unsigned long long)idx,
                 (unsigned long long)emitted);
    if (n < 0 || (size_t)n >= RESP_CAP - off) {
        free(resp);
        return -1;
    }
    off += (size_t)n;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_LIST_DIR_ACK, 0, trace_id,
                    resp, (uint64_t)off);
    free(resp);
    return rc;
}

/* ── FS_HASH handler ─────────────────────────────────────────────────────────
 *
 * Computes the BLAKE3 hash of a single file on the PS5 and returns the
 * 32-byte digest as hex, plus the file's size for consistency checks.
 * Used by the desktop app's "Safe" resume mode to reconcile local-vs-
 * remote content when size equality alone isn't enough.
 *
 * Request JSON:  `{"path":"/abs/path/to/file"}`
 * Response JSON: `{"path":"...","size":N,"hash":"<64-hex-chars>"}`
 *
 * Safety: path must be absolute and must not contain `..`. Read access
 * is gated by the kernel's own permissions; no prefix allowlist.
 *
 * Performance: streams the file in 64 KiB chunks — doesn't buffer the
 * whole file in memory. A 1 GiB file hashes in ~2-3 s on PS5 UFS. For
 * large trees the client should be judicious about how often it calls
 * this (Safe mode only, and only after Fast already filtered by size).
 */
static int handle_fs_hash(runtime_state_t *state, int client_fd,
                           uint64_t trace_id,
                           const char *request_body, uint64_t body_len) {
    /* Per-function stack budget is tight on the PS5 mgmt thread — keep
     * large buffers on the heap. An earlier version had `buf[65536]` on
     * stack which crashed the payload when Safe-mode reconcile called
     * FS_HASH in rapid succession. Local allocations here are all small
     * (<1 KiB combined); the 64 KiB read buffer + the blake3_hasher
     * live on the heap. */
    enum { READ_BUF_SIZE = 65536 };
    char path[512];
    char resp[256];
    char hex[BLAKE3_OUT_LEN * 2 + 1];
    blake3_hasher *hasher = NULL;
    unsigned char *buf = NULL;
    uint8_t digest[BLAKE3_OUT_LEN];
    struct stat st;
    ssize_t r;
    int fd = -1;
    int n;
    int rc;
    static const char hexchars[] = "0123456789abcdef";
    size_t i;

    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
    }
    /* Use the component-aware dotdot check so legitimate filenames
     * like "save..bak" (allowed) pass through; the bare strstr "..":
     * substring rejected those by mistake. is_path_allowed already
     * uses the same component check internally — wraps with the
     * writable-roots allowlist for consistency with other handlers. */
    if (!path[0] || !is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_bad_path", 16);
    }
    if (stat(path, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_stat_failed", 19);
    }
    if (!S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_not_regular_file", 24);
    }
    buf = (unsigned char *)malloc(READ_BUF_SIZE);
    hasher = (blake3_hasher *)malloc(sizeof(*hasher));
    if (!buf || !hasher) {
        free(buf);
        free(hasher);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_oom", 11);
    }
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        free(buf);
        free(hasher);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_open_failed", 19);
    }
    blake3_hasher_init(hasher);
    for (;;) {
        r = read(fd, buf, READ_BUF_SIZE);
        if (r < 0) {
            if (errno == EINTR) continue;
            close(fd);
            free(buf);
            free(hasher);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_hash_read_failed", 19);
        }
        if (r == 0) break;
        blake3_hasher_update(hasher, buf, (size_t)r);
    }
    close(fd);
    blake3_hasher_finalize(hasher, digest, BLAKE3_OUT_LEN);
    free(buf);
    free(hasher);

    for (i = 0; i < BLAKE3_OUT_LEN; i++) {
        hex[2 * i]     = hexchars[(digest[i] >> 4) & 0xf];
        hex[2 * i + 1] = hexchars[digest[i] & 0xf];
    }
    hex[BLAKE3_OUT_LEN * 2] = '\0';

    {
        char path_esc[1024];
        json_escape_into(path, path_esc, sizeof(path_esc));
        n = snprintf(resp, sizeof(resp),
                     "{\"path\":\"%s\",\"size\":%llu,\"hash\":\"%s\"}",
                     path_esc, (unsigned long long)st.st_size, hex);
    }
    if (n < 0 || (size_t)n >= sizeof(resp)) return -1;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_HASH_ACK, 0, trace_id,
                    resp, (uint64_t)n);
    return rc;
}

/* ── Writable-root allowlist ─────────────────────────────────────────────────
 *
 * Destructive FS ops (delete/move/chmod/mkdir) must never touch system
 * paths. The rule: absolute paths only, no `..`, and the path must
 * start with one of the writable roots below. A user running `rm -rf`
 * on `/system` would be bad; this allowlist is the guard.
 *
 *   /data/...         — internal storage user area
 *   /user/...         — per-user profile + save data
 *   /mnt/ext<digit>/  — external SSDs
 *   /mnt/usb<digit>/  — USB-attached storage
 *
 * Intentionally excludes /system, / (root), /dev, /tmp. If a PS5 variant
 * exposes more writable roots, extend `is_path_allowed`. */
/* Reject any path component equal to `..` or `.`. Component-scoped
 * (not substring) so legitimate filenames like `My..Game` or `..rc1`
 * are accepted. The substring-based `strstr(p, "..")` we used to do
 * over-rejected those.
 *
 * Returns 1 if the path contains a forbidden component, 0 if clean. */
static int path_has_dotdot_component(const char *p) {
    const char *seg = p;
    while (*seg) {
        if (*seg == '/') { seg++; continue; }
        const char *end = seg;
        while (*end && *end != '/') end++;
        size_t len = (size_t)(end - seg);
        if (len == 1 && seg[0] == '.') return 1;
        if (len == 2 && seg[0] == '.' && seg[1] == '.') return 1;
        seg = end;
    }
    return 0;
}

/* The lexical half of is_path_allowed: pure string check, no I/O.
 * Extracted so both the input path AND the realpath()-resolved
 * canonical form can be re-validated against the same rules. */
static int is_path_lexically_allowed(const char *p) {
    if (!p || p[0] != '/') return 0;
    if (path_has_dotdot_component(p)) return 0;
    /* Accept exactly /data or /data/... */
    if (strcmp(p, "/data") == 0 || strncmp(p, "/data/", 6) == 0) return 1;
    if (strcmp(p, "/user") == 0 || strncmp(p, "/user/", 6) == 0) return 1;
    /* /mnt/ext<digit>[/...] and /mnt/usb<digit>[/...] */
    if (strncmp(p, "/mnt/ext", 8) == 0 || strncmp(p, "/mnt/usb", 8) == 0) {
        const char *q = p + 8;
        if (*q < '0' || *q > '9') return 0;
        q++;
        /* Optionally a second digit, then either end-of-string or '/'. */
        if (*q >= '0' && *q <= '9') q++;
        return *q == '\0' || *q == '/';
    }
    /* /mnt/ps5upload/<name>[/...] — we create these via FS_MOUNT and
     * surface them in FS_LIST_VOLUMES, so destructive ops (delete/move/
     * chmod/copy/mkdir/read) need to apply to their contents too. Without
     * this, the File System tab could list files inside a mounted image
     * but every edit would hit "path not allowed". The base dir itself
     * (/mnt/ps5upload or /mnt/ps5upload/) is deliberately excluded —
     * callers should target a specific mount's subtree. */
    if (strncmp(p, "/mnt/ps5upload/", 15) == 0 && p[15] != '\0') return 1;
    /* /mnt/shadowmnt[/...] — ShadowMount+ mounts game disc images here
     * (read-only). The File System browser lists these, so reading/
     * downloading files inside them (eboot.bin, sce_sys/, the title
     * JSONs) must be allowed too — otherwise a user can browse a mounted
     * game but every file download fails with fs_read_path_not_allowed.
     * Allow the mount root itself (for listing) and any subpath. Writes
     * to the read-only mount fail at the syscall level regardless. */
    if (strcmp(p, "/mnt/shadowmnt") == 0 ||
        strncmp(p, "/mnt/shadowmnt/", 15) == 0)
        return 1;
    return 0;
}

int is_path_allowed(const char *p) {
    if (!is_path_lexically_allowed(p)) return 0;
    /* (2.9.0) Symlink-escape guard. The lexical check above confirms
     * the path STARTS with an allowed root, but if any component
     * along the path is a symlink that resolves OUTSIDE the allowlist
     * (e.g. /mnt/ps5upload/usermount/evil → /system_ex), the
     * subsequent open()/unlink()/etc. follows the symlink and
     * operates on the forbidden target. Realistic when a user mounts
     * a .ffpkg from an untrusted source — the image is the
     * attacker's data and UFS supports symlinks. Same CWE-59 class
     * as CVE-2007-2374.
     *
     * realpath() resolves all symlinks and collapses any embedded
     * dotdots. If the canonical form fails the lexical check, the
     * path was escaping via a symlink — refuse.
     *
     * realpath fails (returns NULL) when any component along the
     * path doesn't exist yet — common for FS_WRITE / mkdir paths
     * that are about to create the target. For those there's no
     * symlink to follow yet, so accept based on the lexical decision
     * we already passed. The first time a real file appears at this
     * path, subsequent calls go through the realpath check above
     * and reject any symlink the writer planted. */
    char resolved[PATH_MAX];
    if (realpath(p, resolved) == NULL) {
        return 1;
    }
    if (!is_path_lexically_allowed(resolved)) {
        fprintf(stderr,
                "[payload2] is_path_allowed REJECTED: %s resolves to %s "
                "(symlink escape)\n",
                p, resolved);
        return 0;
    }
    return 1;
}

/* Recursively remove `path`. Descends directories, unlinks regular files
 * and symlinks, rmdir's empty directories. Depth cap prevents a
 * pathological symlink loop from exhausting the thread stack. */
/* Forward declarations: rm_rf_op below uses fs_op_* helpers + the
 * recursive_size walker, all of which are defined further down the
 * file (next to the FS_OP_STATUS handler and cp_rf_op respectively).
 * Inserting prototypes here keeps the rm_rf cluster in its existing
 * spot rather than reshuffling 200 lines of unrelated code. */
static int  fs_op_cancel_pending(int idx);
static void fs_op_progress(int idx, uint64_t delta);

/* Op-aware recursive remove. Mirrors rm_rf but, when `op_idx >= 0`,
 * checks the cancel flag periodically (between siblings) and reports
 * progress (bytes freed) to the in-flight ops table after each unlink.
 *
 * Returns:
 *   0  success
 *  -1  hard error (continues best-effort like rm_rf does)
 *  -2  cancelled mid-flight (only when op_idx >= 0 and the engine
 *      called FS_OP_CANCEL on this op_id)
 *
 * Cancel is checked between directory entries — same cadence as
 * cp_rf_op, which is fine because each unlink/rmdir on PS5 UFS is
 * sub-millisecond, so the user-visible cancel latency stays in the
 * "tens of ms" range even on small-file-heavy regimes (PPSA01342:
 * 223k files).
 *
 * Progress unit is bytes-freed: we have st.st_size from the lstat we
 * already did to decide regular-file vs directory, so adding it to
 * the op counter is free. Bytes match the unit fs_copy uses, so the
 * existing engine + UI plumbing renders delete progress without
 * changes (total_bytes from recursive_size, bytes_copied from this
 * accumulator). */
static int rm_rf_op(const char *path, int depth, int op_idx) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0;

    if (depth > 64) return -1;
    if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) return -2;
    if (lstat(path, &st) != 0) {
        /* Already gone (ENOENT) is SUCCESS, not failure. A concurrent boot
         * sweep, or the caller's own earlier delete, may have removed it —
         * surfacing that as fs_delete_failed turned a benign race (e.g. the
         * staged-pkg auto-delete after an install) into a scary user error. */
        if (errno == ENOENT) return 0;
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        /* Regular file / symlink / device. unlink() works for all. */
        if (unlink(path) != 0) {
            if (errno == ENOENT) return 0; /* already gone → ok */
            /* EBUSY: Sony's async installer can still hold a just-installed
             * .pkg open when auto-delete-after-install fires. Retry briefly
             * (≤1s) before declaring failure rather than failing the click. */
            if (errno == EBUSY) {
                int freed = 0;
                for (int i = 0; i < 10; i++) {
                    usleep(100000); /* 100 ms */
                    if (unlink(path) == 0 || errno == ENOENT) {
                        freed = 1;
                        break;
                    }
                    if (errno != EBUSY) break;
                }
                if (!freed) return -1;
            } else {
                return -1;
            }
        }
        if (op_idx >= 0 && S_ISREG(st.st_mode)) {
            fs_op_progress(op_idx, (uint64_t)st.st_size);
        }
        return 0;
    }
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) { rc = -2; break; }
        int n = snprintf(sub, sizeof(sub), "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sub)) { rc = -1; break; }
        int sub_rc = rm_rf_op(sub, depth + 1, op_idx);
        if (sub_rc == -2) { rc = -2; break; }   /* propagate cancel */
        if (sub_rc != 0) { rc = -1; /* keep going; best effort */ }
    }
    closedir(d);
    /* Only rmdir if we weren't cancelled — leaving the dir avoids the
     * surprising case where a user hits Stop and the top-level dir
     * vanishes anyway because rmdir succeeded on the now-empty subtree
     * we already cleared. The fs_copy_cancelled cleanup pattern in
     * handle_fs_delete also relies on this: a partial tree is
     * acceptable, but the entry the user clicked Stop on stays so
     * they can see what's left. */
    /* ENOENT here too means the directory is already gone — treat as success. */
    if (rc != -2 && rmdir(path) != 0 && errno != ENOENT) rc = -1;
    return rc;
}

static int rm_rf(const char *path, int depth) {
    return rm_rf_op(path, depth, -1);
}

/* Recursive chmod on `path`. Descends dirs. Same depth cap as rm_rf. */
static int chmod_rf(const char *path, mode_t mode, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0;

    if (depth > 64) return -1;
    if (lstat(path, &st) != 0) return -1;
    /* chmod first, then descend — so even if recursion fails partially
     * we've at least updated the top. */
    if (chmod(path, mode) != 0) rc = -1;
    if (!S_ISDIR(st.st_mode)) return rc;
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        int n = snprintf(sub, sizeof(sub), "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sub)) { rc = -1; break; }
        if (chmod_rf(sub, mode, depth + 1) != 0) rc = -1;
    }
    closedir(d);
    return rc;
}

/* Recursive copy from `src` to `dst`. Descends dirs, copies regular
 * files byte-for-byte with mode bits preserved. Refuses to overwrite
 * an existing `dst` — the caller (FS_COPY handler) pre-checks. Depth
 * cap matches rm_rf/chmod_rf to bound symlink-loop stack usage.
 *
 * Symlinks are recreated as symlinks (not dereferenced) — matches
 * `cp -R` semantics. Special files (devices, sockets, fifos) are
 * skipped with a log line; they don't belong in a user data tree.
 *
 * Throughput: 4 MiB IOs + posix_fadvise(SEQUENTIAL) hint + up-front
 * posix_fallocate so the FS reserves contiguous extents. The previous
 * 64 KiB buffer required ~540 thousand read/write syscall pairs for
 * a 33 GiB file — syscall overhead alone capped throughput at
 * ~22 MiB/s on PS5 NVMe, roughly 10× below the disk-limited rate.
 * 4 MiB buffer × kernel-readahead hint should bring us close to the
 * sequential-IO ceiling (~200 MiB/s observed on internal NVMe).
 *
 * The buffer is allocated once at the cp_rf entry that triggers the
 * S_ISREG branch — recursive descent re-allocates per file. Trading
 * peak RSS for throughput here is fine because cp_rf isn't run on
 * the FTX2 transfer port that needs its own 32 MiB shard buffer. */
/* ── In-flight fs op tracking (progress + cancel) ─────────────────────────────
 *
 * Long-running fs ops (cp_rf and the copy fallback inside the EXDEV
 * move path) update bytes_copied as they go, and check cancel_requested
 * between buffer flushes. The engine polls FS_OP_STATUS on a separate
 * mgmt-port connection to surface progress to the user, and sends
 * FS_OP_CANCEL when the Stop button fires.
 *
 * Indexed by op_id, which is the trace_id of the originating FS_COPY
 * frame. Bounded at MAX_FS_OPS; allocation is strict first-fit and
 * fails (returns -1 → fs_copy_too_many_inflight) when all slots are
 * busy, rather than evicting an in-flight op. Eviction would silently
 * blind the client's progress poll on whichever op got displaced and
 * surface as a confusing "404 op_id not found" mid-copy, so we'd
 * rather refuse the new op and let the client retry once a slot frees.
 *
 * Single mutex protects the table; per-op fields are read/written
 * under the same lock. The lock is taken briefly inside cp_rf's
 * per-buffer loop (every 4 MiB write) — negligible overhead vs disk
 * I/O, and cleaner than a per-slot mutex for state this small. */
#define MAX_FS_OPS 4

typedef struct {
    int in_use;
    uint64_t op_id;
    char kind[16];          /* "fs_copy", "fs_move", … */
    char from[512];
    char to[512];
    uint64_t total_bytes;   /* 0 if unknown (caller failed to walk) */
    uint64_t bytes_copied;
    int cancel_requested;
    uint64_t started_at_us;
} fs_op_state_t;

static fs_op_state_t g_fs_ops[MAX_FS_OPS];
static pthread_mutex_t g_fs_ops_mtx = PTHREAD_MUTEX_INITIALIZER;

/* Claim a slot for a new op. Returns the slot index on success or
 * -1 if all slots are full (caller should fail the FS_COPY frame
 * rather than block — a pathological 4-concurrent-fs_copy state
 * shouldn't happen in normal use; the cap prevents resource leaks
 * if it does). Caller must call fs_op_release when done.
 *
 * Stuck-op watchdog: if all slots are in_use AND any are older than
 * FS_OP_STUCK_THRESHOLD_US, reclaim the oldest-started one with a
 * log warning. This handles the "worker thread crashed without
 * calling fs_op_release" case which would otherwise permanently leak
 * the slot. The threshold is intentionally generous (24h) — a
 * legitimate copy of a 28 GiB folder over USB takes <30 minutes;
 * legitimate delete of a 200k-file game takes <10 minutes. Any op
 * still claiming a slot 24h after start is hung. */
#define FS_OP_STUCK_THRESHOLD_US (24ULL * 60ULL * 60ULL * 1000000ULL)
static int fs_op_register(uint64_t op_id, const char *kind,
                          const char *from, const char *to,
                          uint64_t total_bytes) {
    int i;
    int idx = -1;
    uint64_t now = now_us();
    pthread_mutex_lock(&g_fs_ops_mtx);
    for (i = 0; i < MAX_FS_OPS; i++) {
        if (!g_fs_ops[i].in_use) {
            idx = i;
            break;
        }
    }
    if (idx < 0) {
        /* No free slot — try the watchdog reclaim. Pick the
         * oldest-started in_use slot; if it's past threshold,
         * reclaim. Don't reclaim a recently-started slot even if
         * we're at full capacity — that's normal load, not a leak. */
        int oldest_idx = -1;
        uint64_t oldest_start = UINT64_MAX;
        for (i = 0; i < MAX_FS_OPS; i++) {
            if (g_fs_ops[i].in_use && g_fs_ops[i].started_at_us < oldest_start) {
                oldest_start = g_fs_ops[i].started_at_us;
                oldest_idx = i;
            }
        }
        if (oldest_idx >= 0 && now > oldest_start &&
            (now - oldest_start) > FS_OP_STUCK_THRESHOLD_US) {
            fprintf(stderr,
                    "[payload2] fs_op watchdog: reclaiming stuck slot %d "
                    "(op_id=%llu kind=%s, age=%llu s) — likely a worker "
                    "crashed without releasing\n",
                    oldest_idx,
                    (unsigned long long)g_fs_ops[oldest_idx].op_id,
                    g_fs_ops[oldest_idx].kind,
                    (unsigned long long)((now - oldest_start) / 1000000ULL));
            idx = oldest_idx;
            /* Fall through to the normal init below. */
        }
    }
    if (idx >= 0) {
        memset(&g_fs_ops[idx], 0, sizeof(g_fs_ops[idx]));
        g_fs_ops[idx].in_use = 1;
        g_fs_ops[idx].op_id = op_id;
        snprintf(g_fs_ops[idx].kind, sizeof(g_fs_ops[idx].kind), "%s", kind);
        snprintf(g_fs_ops[idx].from, sizeof(g_fs_ops[idx].from), "%s", from ? from : "");
        snprintf(g_fs_ops[idx].to,   sizeof(g_fs_ops[idx].to),   "%s", to   ? to   : "");
        g_fs_ops[idx].total_bytes = total_bytes;
        g_fs_ops[idx].started_at_us = now;
    }
    pthread_mutex_unlock(&g_fs_ops_mtx);
    return idx;
}

static void fs_op_release(int idx) {
    if (idx < 0 || idx >= MAX_FS_OPS) return;
    pthread_mutex_lock(&g_fs_ops_mtx);
    memset(&g_fs_ops[idx], 0, sizeof(g_fs_ops[idx]));
    pthread_mutex_unlock(&g_fs_ops_mtx);
}

/* Bump the bytes counter by `delta`. Hot path — called once per
 * COPY_BUF (4 MiB) write inside cp_rf. */
static void fs_op_progress(int idx, uint64_t delta) {
    if (idx < 0 || idx >= MAX_FS_OPS) return;
    pthread_mutex_lock(&g_fs_ops_mtx);
    if (g_fs_ops[idx].in_use) {
        g_fs_ops[idx].bytes_copied += delta;
    }
    pthread_mutex_unlock(&g_fs_ops_mtx);
}

/* Patch in the recursive_size pre-walk total once it completes. We
 * register the slot *before* walking now (so the engine's status
 * poller can find the op while the walk runs); this fills in the
 * total_bytes field afterward without re-acquiring the slot. The
 * client renders bytes_copied/total_bytes; total_bytes==0 means
 * "still scanning" and the UI suppresses the percentage until the
 * walk completes — far better than the old behavior where the
 * 250 ms poll-start delay routinely beat a slow recursive walk and
 * latched a "your payload is broken" banner on the very first poll. */
static void fs_op_set_total(int idx, uint64_t total) {
    if (idx < 0 || idx >= MAX_FS_OPS) return;
    pthread_mutex_lock(&g_fs_ops_mtx);
    if (g_fs_ops[idx].in_use) {
        g_fs_ops[idx].total_bytes = total;
    }
    pthread_mutex_unlock(&g_fs_ops_mtx);
}

/* Atomic-ish read of the cancel flag. Called from cp_rf's inner loop
 * before each read() so an inflight 28 GiB copy notices the cancel
 * within one COPY_BUF (4 MiB) of progress. */
static int fs_op_cancel_pending(int idx) {
    int c;
    if (idx < 0 || idx >= MAX_FS_OPS) return 0;
    pthread_mutex_lock(&g_fs_ops_mtx);
    c = g_fs_ops[idx].in_use ? g_fs_ops[idx].cancel_requested : 0;
    pthread_mutex_unlock(&g_fs_ops_mtx);
    return c;
}

/* Snapshot for FS_OP_STATUS. Returns 0 on found, -1 on no-such-op. */
static int fs_op_snapshot(uint64_t op_id, fs_op_state_t *out) {
    int i;
    int found = -1;
    pthread_mutex_lock(&g_fs_ops_mtx);
    for (i = 0; i < MAX_FS_OPS; i++) {
        if (g_fs_ops[i].in_use && g_fs_ops[i].op_id == op_id) {
            *out = g_fs_ops[i];
            found = 0;
            break;
        }
    }
    pthread_mutex_unlock(&g_fs_ops_mtx);
    return found;
}

/* Set the cancel flag on the matching op. Returns 0 on found, -1 on
 * no-such-op. The op handler picks this up at its next loop check. */
static int fs_op_set_cancel(uint64_t op_id) {
    int i;
    int found = -1;
    pthread_mutex_lock(&g_fs_ops_mtx);
    for (i = 0; i < MAX_FS_OPS; i++) {
        if (g_fs_ops[i].in_use && g_fs_ops[i].op_id == op_id) {
            g_fs_ops[i].cancel_requested = 1;
            found = 0;
            break;
        }
    }
    pthread_mutex_unlock(&g_fs_ops_mtx);
    return found;
}

/* Recursive byte-count of a path. Used by handle_fs_copy /
 * handle_fs_move's copy-fallback to populate total_bytes before
 * starting the work, so the engine's progress poll can show a
 * percentage. Cheap relative to the copy itself (one stat per file).
 * Returns 0 and writes total to *out on success; -1 on any stat
 * failure (path missing, permission, etc.). */
/* Depth cap matching rm_rf_op / chmod_rf / remove_recursive_path. Bounds
 * stack usage against symlink loops or pathological nesting in the source
 * tree we're sizing. Without it, a large stat-walk could stack-overflow
 * the payload before producing a clean error.
 *
 * `op_idx` is the in-flight op slot (or -1 if untracked). When >= 0,
 * the walker checks the cancel flag periodically so a Stop click
 * during the pre-walk takes effect quickly, instead of waiting for
 * the much longer cp_rf phase to finish first. Returns -2 on cancel
 * (matches cp_rf_op's convention). */
#define RECURSIVE_SIZE_MAX_DEPTH 64
static int recursive_size_inner(const char *path, uint64_t *out,
                                 int depth, int op_idx) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    if (depth > RECURSIVE_SIZE_MAX_DEPTH) {
        fprintf(stderr,
                "[payload2] recursive_size: depth cap %d hit at %s\n",
                RECURSIVE_SIZE_MAX_DEPTH, path);
        return -1;
    }
    /* Cancel check at directory boundaries — same cadence as
     * rm_rf_op / cp_rf_op. Per-file granularity isn't worth it for
     * stat walks (each stat is sub-microsecond on warm cache). */
    if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) return -2;
    if (lstat(path, &st) != 0) return -1;
    if (S_ISREG(st.st_mode)) {
        *out += (uint64_t)st.st_size;
        return 0;
    }
    if (!S_ISDIR(st.st_mode)) {
        /* symlinks/specials don't contribute bytes. */
        return 0;
    }
    d = opendir(path);
    if (!d) return -1;
    int rc = 0;
    while ((e = readdir(d)) != NULL) {
        char sub[1024];
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (snprintf(sub, sizeof(sub), "%s/%s", path, e->d_name) >= (int)sizeof(sub)) {
            /* A truncated child path means the copy will hit the same
             * limit and bail; reporting success here would lie about
             * total_bytes and make the progress bar drift past 100%.
             * Surface the failure now so the caller fails fast with
             * fs_copy_walk_failed rather than mid-copy. */
            rc = -1;
            break;
        }
        int sub_rc = recursive_size_inner(sub, out, depth + 1, op_idx);
        if (sub_rc != 0) {
            rc = sub_rc; /* propagate -1 (error) or -2 (cancel) */
            break;
        }
    }
    closedir(d);
    return rc;
}

static int recursive_size_op(const char *path, uint64_t *out, int op_idx) {
    return recursive_size_inner(path, out, 0, op_idx);
}

/* ── Async-write file copier (parallel read + write) ──────────────────────────
 *
 * The original cp_rf body did a sequential `read(sfd) → write(dfd)`
 * loop. For an internal copy from /data (NVMe ~800 MiB/s) to
 * /mnt/usb0 (USB exFAT ~30 MiB/s) the read-then-write per-buffer
 * latency adds: throughput is bounded by SUM of the two side rates,
 * not the slower one alone. Concretely a 4 MiB buffer with a 5 ms
 * read + 130 ms write spends ~96% of its time waiting for the write
 * — but the next read is also serialized behind it.
 *
 * This helper runs the write on a worker thread fed by a 2-slot
 * ring buffer. While the worker is writing buf[A] the reader fills
 * buf[B], and they swap. End-to-end throughput becomes
 * `max(read_rate, write_rate)` ≈ the slower side (USB), which is
 * the absolute ceiling — already up from 21 MiB/s observed on the
 * old serial path to whatever the destination's true sustained
 * write speed is.
 *
 * Buffer size bumped to 16 MiB (vs old 4 MiB): reduces per-iteration
 * overhead, gives exFAT/USB a bigger contiguous block to write,
 * and leaves the kernel more flexibility to coalesce. Two of these
 * + the read buffer === ~32 MiB peak per concurrent copy, well
 * under PS5 user-mode RAM budget.
 *
 * Returns: 0 success, -1 error, -2 cancel. */
#define ASYNC_COPY_BUF_BYTES (16u * 1024u * 1024u)

typedef struct {
    pthread_mutex_t mtx;
    pthread_cond_t  cv_filled;   /* reader → writer: slot has data */
    pthread_cond_t  cv_emptied;  /* writer → reader: slot consumed */
    int             dfd;
    const char     *dst_path;    /* for error logging only */
    unsigned char  *bufs[2];
    size_t          slot_len[2]; /* 0 = empty, >0 = bytes to write */
    int             eof[2];      /* 1 = no more data after this slot */
    int             writer_errno; /* sticky; reader bails if non-zero */
    int             shutdown;    /* set on cancel/error to wake the writer */
    pthread_t       tid;
    int             tid_started;
} async_copy_t;

static void *async_copy_writer_thread(void *arg) {
    async_copy_t *w = (async_copy_t *)arg;
    int slot = 0;
    /* Bytes written since the last periodic fsync. A cross-volume move
     * (e.g. external SSD -> internal) is a local-to-local byte copy with
     * NO network to pace the writer, so dirty pages pile up far faster
     * than during an upload. Left unbounded, a multi-GB game copy grows
     * the kernel dirty-page backlog into the GBs and the PS5 kernel
     * panics under the writeback/VM pressure (observed: console shutdown,
     * lost jailbreak, mid-copy of a ~50 GB title). fsync drains the dirty
     * pages to clean ones on a fixed cadence so the backlog stays bounded
     * -- the same fix piped_writer_thread() uses for huge uploads. See
     * PS5UPLOAD2_WRITEBACK_FSYNC_BYTES. */
    uint64_t since_fsync = 0;
    for (;;) {
        size_t len;
        int eof;
        pthread_mutex_lock(&w->mtx);
        while (w->slot_len[slot] == 0 && !w->eof[slot] && !w->shutdown) {
            pthread_cond_wait(&w->cv_filled, &w->mtx);
        }
        if (w->shutdown) {
            pthread_mutex_unlock(&w->mtx);
            return NULL;
        }
        len = w->slot_len[slot];
        eof = w->eof[slot];
        pthread_mutex_unlock(&w->mtx);

        if (len > 0) {
            ssize_t written = 0;
            while ((size_t)written < len) {
                ssize_t wr = write(w->dfd, w->bufs[slot] + written, len - (size_t)written);
                if (wr < 0) {
                    if (errno == EINTR) continue;
                    pthread_mutex_lock(&w->mtx);
                    w->writer_errno = errno;
                    pthread_cond_broadcast(&w->cv_emptied);
                    pthread_mutex_unlock(&w->mtx);
                    fprintf(stderr,
                            "[payload2] async_copy: write(%s) failed errno=%d\n",
                            w->dst_path, errno);
                    return NULL;
                }
                written += wr;
            }
            /* Bound the dirty-page backlog: flush on a fixed byte cadence
             * so a huge cross-volume copy can't balloon the kernel's dirty
             * count into the GBs and panic the console. Done before the
             * empty-signal so the reader naturally blocks on a full slot
             * during the flush -- that backpressure is exactly the pacing
             * we want. A flush failure isn't fatal: a real I/O error still
             * surfaces via the write() path above. */
#if PS5UPLOAD2_WRITEBACK_FSYNC_BYTES > 0
            since_fsync += (uint64_t)len;
            if (since_fsync >= PS5UPLOAD2_WRITEBACK_FSYNC_BYTES) {
                (void)fsync(w->dfd);
                since_fsync = 0;
            }
#endif
        }

        pthread_mutex_lock(&w->mtx);
        w->slot_len[slot] = 0;
        pthread_cond_signal(&w->cv_emptied);
        pthread_mutex_unlock(&w->mtx);

        if (eof) return NULL;
        slot = 1 - slot;
    }
}

/* Run an async-write copy of all bytes from `sfd` to `dfd`. Updates
 * the op slot's bytes_copied counter on each completed read so the
 * engine's progress poll sees the value advance in real time. */
static int async_copy_fd(int sfd, int dfd, const char *src_path,
                         const char *dst_path, int op_idx) {
    async_copy_t w;
    memset(&w, 0, sizeof(w));
    w.dfd = dfd;
    w.dst_path = dst_path;
    if (pthread_mutex_init(&w.mtx, NULL) != 0) return -1;
    if (pthread_cond_init(&w.cv_filled, NULL) != 0) {
        pthread_mutex_destroy(&w.mtx);
        return -1;
    }
    if (pthread_cond_init(&w.cv_emptied, NULL) != 0) {
        pthread_cond_destroy(&w.cv_filled);
        pthread_mutex_destroy(&w.mtx);
        return -1;
    }
    w.bufs[0] = (unsigned char *)malloc(ASYNC_COPY_BUF_BYTES);
    w.bufs[1] = (unsigned char *)malloc(ASYNC_COPY_BUF_BYTES);
    if (!w.bufs[0] || !w.bufs[1]) {
        free(w.bufs[0]);
        free(w.bufs[1]);
        pthread_cond_destroy(&w.cv_emptied);
        pthread_cond_destroy(&w.cv_filled);
        pthread_mutex_destroy(&w.mtx);
        return -1;
    }
    if (create_worker_thread(&w.tid, async_copy_writer_thread, &w) != 0) {
        free(w.bufs[0]);
        free(w.bufs[1]);
        pthread_cond_destroy(&w.cv_emptied);
        pthread_cond_destroy(&w.cv_filled);
        pthread_mutex_destroy(&w.mtx);
        return -1;
    }
    w.tid_started = 1;

    int rc = 0;
    int slot = 0;
    off_t total_read = 0;
    for (;;) {
        /* Cancel check between each read — same granularity as
         * before (one buffer = 16 MiB now). */
        if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) {
            rc = -2;
            break;
        }
        /* Wait for our slot to be empty. The writer signals
         * cv_emptied when it finishes a slot; we also wake on a
         * writer error so we don't deadlock against a dead writer. */
        pthread_mutex_lock(&w.mtx);
        while (w.slot_len[slot] != 0 && w.writer_errno == 0) {
            pthread_cond_wait(&w.cv_emptied, &w.mtx);
        }
        if (w.writer_errno != 0) {
            pthread_mutex_unlock(&w.mtx);
            rc = -1;
            break;
        }
        pthread_mutex_unlock(&w.mtx);

        ssize_t r = read(sfd, w.bufs[slot], ASYNC_COPY_BUF_BYTES);
        if (r < 0) {
            if (errno == EINTR) continue;
            fprintf(stderr,
                    "[payload2] async_copy: read(%s) failed at offset %lld errno=%d\n",
                    src_path, (long long)total_read, errno);
            rc = -1;
            break;
        }
        if (r == 0) {
            /* Hand an EOF marker to the writer so it exits cleanly
             * instead of waiting forever on the next slot. */
            pthread_mutex_lock(&w.mtx);
            w.eof[slot] = 1;
            pthread_cond_signal(&w.cv_filled);
            pthread_mutex_unlock(&w.mtx);
            break;
        }
        total_read += r;
        if (op_idx >= 0) fs_op_progress(op_idx, (uint64_t)r);

        pthread_mutex_lock(&w.mtx);
        w.slot_len[slot] = (size_t)r;
        pthread_cond_signal(&w.cv_filled);
        pthread_mutex_unlock(&w.mtx);

        slot = 1 - slot;
    }

    if (rc != 0) {
        /* Tell the writer to give up on whatever's queued. */
        pthread_mutex_lock(&w.mtx);
        w.shutdown = 1;
        pthread_cond_broadcast(&w.cv_filled);
        pthread_mutex_unlock(&w.mtx);
    }
    pthread_join(w.tid, NULL);
    if (rc == 0 && w.writer_errno != 0) rc = -1;

    free(w.bufs[0]);
    free(w.bufs[1]);
    pthread_cond_destroy(&w.cv_emptied);
    pthread_cond_destroy(&w.cv_filled);
    pthread_mutex_destroy(&w.mtx);
    return rc;
}

/* Recursive copy with progress + cancel hooks. `op_idx` is a slot
 * into g_fs_ops (or -1 to disable the hooks for callers that don't
 * need cancellation/progress visibility). Returns:
 *   0   = success
 *  -1   = generic error (open, read, write, mkdir, etc.)
 *  -2   = cancel requested (distinguishable from -1 so the caller
 *         can return FS_OP_CANCEL_ACK instead of FS_COPY_ERROR). */
static int cp_rf_op(const char *src, const char *dst, int depth, int op_idx) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub_src[1024];
    char sub_dst[1024];
    int rc = 0;

    if (depth > 64) return -1;
    if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) return -2;
    if (lstat(src, &st) != 0) return -1;

    if (S_ISLNK(st.st_mode)) {
        char target[1024];
        ssize_t len = readlink(src, target, sizeof(target) - 1);
        if (len < 0) return -1;
        target[len] = '\0';
        if (symlink(target, dst) != 0) return -1;
        return 0;
    }

    if (S_ISREG(st.st_mode)) {
        int sfd = open(src, O_RDONLY);
        if (sfd < 0) return -1;
        /* Open with 0777 (not source mode) so the destination is launch-
         * ready regardless of what the source file's mode bits looked
         * like. Sony's loader rejects game files lacking world-exec with
         * CE-107750-0 ("can't start game or app"). Copying a folder that
         * was previously uploaded with a pre-2.16.1 payload would inherit
         * the 0644 source modes and reproduce that loader failure on the
         * fresh copy — exactly the bug umask(0) + per-file fchmod(0777)
         * was added to fix for the upload path. fchmod after open keeps
         * the bits even if the dst FS's umask handling differs. */
        int dfd = open(dst, O_WRONLY | O_CREAT | O_EXCL, 0777);
        if (dfd < 0) { close(sfd); return -1; }
        (void)fchmod(dfd, 0777);
        /* Tell the kernel: we'll read this file front-to-back, prefetch
         * aggressively. FreeBSD's readahead defaults to 64 KiB which is
         * what was throttling us. With the SEQUENTIAL hint the kernel
         * scales the readahead window up to handle large IOs. Failures
         * are non-fatal — just lose the perf hint. */
        (void)posix_fadvise(sfd, 0, 0, POSIX_FADV_SEQUENTIAL);
        /* Reserve contiguous extents on the dst FS so the writer
         * doesn't interleave block-allocation with data writes. UFS2
         * (PS5 internal) honours this; exfatfs (USB exFAT) does too,
         * via the fallocate emulation path.
         *
         * Most failures are non-fatal (FS doesn't support the hint —
         * EINVAL/EOPNOTSUPP on tmpfs etc.) — fall through to the
         * lazy-allocation path. But ENOSPC and EFBIG are early
         * disk-full / file-too-big signals worth surfacing now
         * instead of after we've written half the file: the kernel
         * already proved it can't take the whole thing. Fail fast so
         * the caller sees a specific "destination is full" error
         * rather than a generic mid-copy write failure at some
         * random offset. */
        if (st.st_size > 0) {
            int falloc_rc = posix_fallocate(dfd, 0, st.st_size);
            if (falloc_rc == ENOSPC || falloc_rc == EFBIG) {
                fprintf(stderr,
                        "[payload2] fs_copy: posix_fallocate(%lld bytes) returned %d for %s — bailing early\n",
                        (long long)st.st_size, falloc_rc, dst);
                close(sfd);
                close(dfd);
                /* Best-effort cleanup of the empty/zero-extent dst
                 * the open(O_CREAT) just produced — don't leave a
                 * 0-byte ghost file. */
                (void)unlink(dst);
                return -1;
            }
            /* Other return values (0 success, EINVAL/EOPNOTSUPP,
             * etc.) all fall through and let the read/write loop
             * proceed with lazy allocation. */
            /* If the preallocation succeeded for a large file, drain it
             * now. On filesystems whose fallocate zero-fills through the
             * buffer cache (rather than reserving bare extents), a
             * multi-GB preallocation would otherwise hand the copy loop a
             * huge dirty-page backlog before the first byte is even read
             * -- the same writeback pressure that can panic the console.
             * A pure extent-reserving fallocate leaves nothing dirty, so
             * this is a cheap no-op there. Threshold mirrors the copy
             * loop's flush cadence. */
            if (falloc_rc == 0 &&
                (uint64_t)st.st_size >= PS5UPLOAD2_WRITEBACK_FSYNC_BYTES) {
                (void)fsync(dfd);
            }
        }
        /* Hand the actual byte movement to the async-write helper:
         * one writer thread + double-buffered 16 MiB slots so read
         * and write run in parallel, capping throughput at the
         * slower side instead of read+write summed. */
        rc = async_copy_fd(sfd, dfd, src, dst, op_idx);
        close(sfd);
        close(dfd);
        if (rc == -2) {
            /* Cancelled mid-file — drop the partial dst so a retry
             * doesn't get blocked by the dest-exists check. */
            (void)unlink(dst);
        } else if (rc != 0) {
            /* Hard error — also drop the partial dst rather than
             * leaving a 0-byte (or partially-written) ghost that
             * would confuse the user. */
            (void)unlink(dst);
        }
        return rc;
    }

    if (!S_ISDIR(st.st_mode)) {
        /* Special file (device/socket/fifo) — skip. User data trees
         * should never contain these, so silently dropping them is
         * safer than failing the whole copy. */
        fprintf(stderr, "[payload2] fs_copy: skipping non-regular %s\n", src);
        return 0;
    }

    /* Directory: mkdir then descend. EEXIST on dst would indicate the
     * pre-check missed something — treat as error so the caller sees
     * partial-state.
     *
     * mkdir with 0777 (not source mode) for the same reason as the
     * regular-file open above: Sony's loader needs world-executable
     * directories for game-folder navigation. chmod after mkdir
     * enforces the mode even if the dst FS's umask handling differs. */
    if (mkdir(dst, 0777) != 0) return -1;
    (void)chmod(dst, 0777);
    d = opendir(src);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (op_idx >= 0 && fs_op_cancel_pending(op_idx)) { rc = -2; break; }
        int n1 = snprintf(sub_src, sizeof(sub_src), "%s/%s", src, e->d_name);
        int n2 = snprintf(sub_dst, sizeof(sub_dst), "%s/%s", dst, e->d_name);
        if (n1 < 0 || (size_t)n1 >= sizeof(sub_src) ||
            n2 < 0 || (size_t)n2 >= sizeof(sub_dst)) { rc = -1; break; }
        int sub_rc = cp_rf_op(sub_src, sub_dst, depth + 1, op_idx);
        if (sub_rc == -2) { rc = -2; break; }   /* propagate cancel */
        if (sub_rc != 0) { rc = -1; /* per-file failure: keep going */ }
    }
    closedir(d);
    return rc;
}

/* ── FS_OP_STATUS / FS_OP_CANCEL handlers ──────────────────────────────────
 *
 * Both take `{"op_id":<u64>}` in the body where op_id is the trace_id
 * of the originating FS_COPY frame. Status returns a JSON snapshot;
 * cancel flips the atomic flag the cp_rf loop checks every 4 MiB.
 *
 * These run on the mgmt-port worker pool (different connection from
 * the FS_COPY handler itself), so the engine can interleave them with
 * the FS_COPY handler's wait for FS_COPY_ACK without serialization. */
static int handle_fs_op_status(int client_fd, uint64_t trace_id,
                                const char *request_body) {
    fs_op_state_t snap;
    /* Buffer must hold the JSON skeleton plus two fully escaped path
     * strings. Each escaped path is up to 1024 bytes (json_escape_into's
     * dst_cap below), so 2*1024 + ~512 for the rest is safe. Previously
     * this was 768 bytes and silently truncated whenever a cross-mount
     * move's two paths summed to more than ~600 bytes — the engine then
     * 502'd with "decode FS_OP_STATUS_ACK body" and the UI lost progress. */
    char body[2560];
    int len;
    uint64_t op_id = request_body
        ? extract_json_uint64_field(request_body, "op_id")
        : 0;
    if (op_id == 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_op_status_missing_op_id", 26);
    }
    if (fs_op_snapshot(op_id, &snap) != 0) {
        /* No matching op — either it finished (engine should stop
         * polling) or never existed. Distinct empty-body ACK with
         * `found:false` lets the engine decide. */
        len = snprintf(body, sizeof(body), "{\"found\":false}");
        if (len < 0) return -1;
        return send_frame(client_fd, FTX2_FRAME_FS_OP_STATUS_ACK, 0,
                          trace_id, body, (uint64_t)len);
    }
    /* Path strings escaped so embedded backslashes/quotes don't
     * corrupt the JSON. The shared json_escape_into helper used
     * elsewhere in this file does the right thing. */
    char from_esc[1024];
    char to_esc[1024];
    json_escape_into(snap.from, from_esc, sizeof(from_esc));
    json_escape_into(snap.to,   to_esc,   sizeof(to_esc));
    /* `cancel_requested` MUST be emitted as a JSON boolean — the
     * engine's `OpStatus.cancel_requested` is declared `bool` and
     * serde rejects integer-for-bool, breaking the entire decode.
     * Same root-cause class as the FS_MOUNT_ACK / PKG_INSTALL_ACK
     * fixes already shipped this round. */
    const char *cancel_str = snap.cancel_requested ? "true" : "false";
    len = snprintf(body, sizeof(body),
                   "{\"found\":true,\"op_id\":%llu,\"kind\":\"%s\","
                   "\"from\":\"%s\",\"to\":\"%s\","
                   "\"total_bytes\":%llu,\"bytes_copied\":%llu,"
                   "\"cancel_requested\":%s}",
                   (unsigned long long)snap.op_id,
                   snap.kind,
                   from_esc, to_esc,
                   (unsigned long long)snap.total_bytes,
                   (unsigned long long)snap.bytes_copied,
                   cancel_str);
    if (len < 0) return -1;
    /* Truncation here would emit malformed JSON (snprintf returns the
     * would-have-been length, not the written length, when clipped).
     * Fail loud with an error frame instead — the engine's bail() path
     * for ERROR is much more informative than "decode … body". */
    if ((size_t)len >= sizeof(body)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_op_status_body_overflow", 26);
    }
    return send_frame(client_fd, FTX2_FRAME_FS_OP_STATUS_ACK, 0,
                      trace_id, body, (uint64_t)len);
}

static int handle_fs_op_cancel(int client_fd, uint64_t trace_id,
                                const char *request_body) {
    char body[64];
    int len;
    uint64_t op_id = request_body
        ? extract_json_uint64_field(request_body, "op_id")
        : 0;
    if (op_id == 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_op_cancel_missing_op_id", 26);
    }
    int rc = fs_op_set_cancel(op_id);
    len = snprintf(body, sizeof(body),
                   "{\"found\":%s}",
                   rc == 0 ? "true" : "false");
    if (len < 0) return -1;
    return send_frame(client_fd, FTX2_FRAME_FS_OP_CANCEL_ACK, 0,
                      trace_id, body, (uint64_t)len);
}

/* ── FS_DELETE handler ───────────────────────────────────────────────────
 *
 * For small-file-heavy game folders (PPSA01342: 223k files / 19k dirs)
 * the recursive walk takes minutes on PS5 UFS. To match fs_copy's UX,
 * we register the op in the in-flight table so the engine's
 * `/api/ps5/fs/op-status` poll can show "freeing X / Y" and the user's
 * Stop button can fire FS_OP_CANCEL to abort early.
 *
 * trace_id == 0 means "no progress/cancel tracking" (legacy callers,
 * single-file unlinks) — skip the registration and call the un-tracked
 * rm_rf so we don't burn a g_fs_ops slot on an instant operation. */
static int handle_fs_delete(runtime_state_t *state, int client_fd,
                             uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) extract_json_string_field(request_body, "path", path, sizeof(path));
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_path_not_allowed", 26);
    }

    /* Untracked fast path: caller doesn't need progress and the
     * payload doesn't need to register a slot. Same behavior as
     * pre-op-id callers. */
    if (trace_id == 0) {
        if (rm_rf(path, 0) != 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_delete_failed", 16);
        }
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);
        return send_frame(client_fd, FTX2_FRAME_FS_DELETE_ACK, 0, trace_id, NULL, 0);
    }

    /* Tracked path. Register the slot *before* walking so the engine's
     * status poller can find the op the moment it asks (same race fix
     * applied to FS_COPY above — small-file-heavy trees walk slower
     * than the poller's 250 ms initial delay, and the old order
     * surfaced the race as a transient parse error that the client
     * mis-attributed to an old payload). All MAX_FS_OPS slots in use
     * → refuse rather than running un-tracked, since the client
     * expects to be able to poll/cancel via the op_id it sent. */
    int op_idx = fs_op_register(trace_id, "fs_delete", path, "", 0);
    if (op_idx < 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_too_many_inflight", 27);
    }
    /* Pre-walk to compute total bytes so the engine's progress poll
     * can show a percentage; same one-stat-per-file cost pattern as
     * fs_copy. `recursive_size_op` returns -1 on stat failure or -2
     * on cancel — surface either now rather than letting rm_rf hit
     * the same file mid-walk. Release the slot on walk failure so a
     * recoverable error doesn't burn one of the MAX_FS_OPS=4 slots. */
    uint64_t total_bytes = 0;
    int walk_rc = recursive_size_op(path, &total_bytes, op_idx);
    if (walk_rc == -2) {
        fs_op_release(op_idx);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_cancelled", 19);
    }
    if (walk_rc != 0) {
        fs_op_release(op_idx);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_walk_failed", 21);
    }
    fs_op_set_total(op_idx, total_bytes);
    int rm_rc = rm_rf_op(path, 0, op_idx);
    fs_op_release(op_idx);

    if (rm_rc == -2) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_cancelled", 19);
    }
    if (rm_rc != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_failed", 16);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_DELETE_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MOVE handler ────────────────────────────────────────────────────
 * rename(2) only works intra-volume on POSIX. Cross-volume moves are supposed
 * to fail with EXDEV — but on this kernel a cross-DEVICE rename (e.g. a USB
 * exFAT file → the internal SSD) does NOT return EXDEV: it KERNEL PANICS and
 * crashes the console (a reproducible hard crash, reported on USB→internal cut
 * & paste). So we must NEVER let rename() run across devices. We compare the
 * source's device id against the destination directory's up front and surface
 * `fs_move_cross_mount` instead — the client then completes the move safely as
 * copy-then-delete (FS_COPY reads/writes bytes, which is cross-volume safe). */
static int handle_fs_move(runtime_state_t *state, int client_fd,
                           uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char from[512], to[512];
    (void)body_len;
    if (!state) return -1;
    from[0] = '\0'; to[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "from", from, sizeof(from));
        extract_json_string_field(request_body, "to", to, sizeof(to));
    }
    if (!is_path_allowed(from) || !is_path_allowed(to)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_move_path_not_allowed", 24);
    }
    /* Cross-device guard — see the header comment. Compare st_dev of the source
     * and of the destination's parent directory; if they differ, refuse the
     * rename (it would panic) and report cross-mount. stat() on USB is safe
     * (FS_COPY stats the same paths). If either stat fails we fall through to
     * rename(), but only when devices can't be compared — a missing source/dest
     * there fails with a normal errno, not the cross-device panic. */
    {
        struct stat sf, sdp;
        char to_dir[512];
        const char *slash = strrchr(to, '/');
        if (slash && slash != to) {
            size_t dlen = (size_t)(slash - to);
            if (dlen >= sizeof(to_dir)) dlen = sizeof(to_dir) - 1;
            memcpy(to_dir, to, dlen);
            to_dir[dlen] = '\0';
        } else {
            to_dir[0] = '/';
            to_dir[1] = '\0';
        }
        if (stat(from, &sf) == 0 && stat(to_dir, &sdp) == 0 &&
            sf.st_dev != sdp.st_dev) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_move_cross_mount", 19);
        }
    }
    if (rename(from, to) != 0) {
        if (errno == EXDEV) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_move_cross_mount", 19);
        }
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_move_failed", 14);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_MOVE_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_CHMOD handler ──────────────────────────────────────────────────── */
static int handle_fs_chmod(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512], mode_str[16];
    uint64_t mode_int;
    int recursive = 0;
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0'; mode_str[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        extract_json_string_field(request_body, "mode", mode_str, sizeof(mode_str));
        /* recursive is an unsigned field: 1 = recurse, 0 = top only. */
        recursive = extract_json_uint64_field(request_body, "recursive") != 0;
    }
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_path_not_allowed", 25);
    }
    if (!mode_str[0]) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_missing_mode", 21);
    }
    /* Parse mode as octal (e.g. "0777", "0644"). Clamp to 0..07777. */
    mode_int = strtoull(mode_str, NULL, 8);
    if (mode_int > 07777) mode_int = 07777;
    int rc;
    if (recursive) {
        rc = chmod_rf(path, (mode_t)mode_int, 0);
    } else {
        rc = chmod(path, (mode_t)mode_int);
    }
    if (rc != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_failed", 15);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_CHMOD_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MKDIR handler ──────────────────────────────────────────────────── */
static int handle_fs_mkdir(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) extract_json_string_field(request_body, "path", path, sizeof(path));
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_path_not_allowed", 25);
    }
    /* ensure_parent_dir walks and mkdir-p's every component. Matches the
     * "create intermediate dirs" behavior callers expect. */
    if (ensure_parent_dir(path) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_parents_failed", 23);
    }
    if (mkdir(path, 0777) != 0 && errno != EEXIST) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_failed", 15);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_MKDIR_ACK, 0, trace_id, NULL, 0);
}

/* READ-ONLY exception to the FS_READ allowlist: the per-user avatar image in
 * Sony's profile cache, so the UI can show the CURRENT avatar before a change.
 * The cache lives under /system_data (outside the writable-root allowlist), but
 * exposing JUST these two PNGs for reading is safe — they're images, the path
 * is fixed-shape (/system_data/priv/cache/profile/0x<HEX>/{avatar,picture}.png),
 * traversal is rejected, and nothing else in the cache (online.json, .dds) is
 * reachable. This does NOT touch is_path_allowed, so writes/copies/deletes to
 * /system_data stay forbidden. */
static int is_profile_avatar_read_path(const char *p) {
    static const char PRE[] = "/system_data/priv/cache/profile/0x";
    if (strncmp(p, PRE, sizeof(PRE) - 1) != 0) return 0;
    if (strstr(p, "..") != NULL) return 0;
    size_t n = strlen(p);
    return (n >= 11 && strcmp(p + n - 11, "/avatar.png") == 0) ||
           (n >= 12 && strcmp(p + n - 12, "/picture.png") == 0);
}

/* ── FS_READ handler ─────────────────────────────────────────────────────
 *
 * Bounded file-read for metadata fetches (param.json, icon0.png). The
 * payload enforces the same writable-root allowlist destructive ops use
 * plus an absolute hard cap (FS_READ_MAX_BYTES) on how much any single
 * read can return. Requests exceeding the cap are silently truncated —
 * callers that care about exact sizes can check the ACK body length
 * against the file's real size via a prior FS_LIST_DIR entry.
 *
 * Request JSON: {"path":"/abs/...","offset":N,"limit":N}  (offset/limit optional)
 * ACK body:     raw file bytes (no framing, no JSON wrap)
 */
static int handle_fs_read(runtime_state_t *state, int client_fd,
                          uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    struct stat st;
    uint64_t offset = 0;
    uint64_t limit = FS_READ_MAX_BYTES;
    unsigned char *buf = NULL;
    ssize_t r;
    int fd = -1;
    int rc;

    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        uint64_t req_offset = extract_json_uint64_field(request_body, "offset");
        uint64_t req_limit  = extract_json_uint64_field(request_body, "limit");
        if (req_offset > 0) offset = req_offset;
        if (req_limit > 0 && req_limit < limit) limit = req_limit;
    }
    if (!is_path_allowed(path) && !is_profile_avatar_read_path(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_path_not_allowed", 24);
    }
    if (stat(path, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_stat_failed", 19);
    }
    if (!S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_not_regular_file", 24);
    }
    /* Clamp limit against actual file size so the ACK body matches what
     * the client can meaningfully consume. Offset past EOF returns an
     * empty ACK (no error) — simplifies paginated reads. */
    uint64_t remaining = 0;
    if ((uint64_t)st.st_size > offset) {
        remaining = (uint64_t)st.st_size - offset;
    }
    if (limit > remaining) limit = remaining;
    if (limit > FS_READ_MAX_BYTES) limit = FS_READ_MAX_BYTES;

    if (limit == 0) {
        /* Zero-length ACK is valid — client sees empty body. */
        return send_frame(client_fd, FTX2_FRAME_FS_READ_ACK, 0, trace_id, NULL, 0);
    }

    buf = (unsigned char *)malloc((size_t)limit);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_oom", 11);
    }
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        free(buf);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_open_failed", 19);
    }
    if (offset > 0 && lseek(fd, (off_t)offset, SEEK_SET) < 0) {
        close(fd);
        free(buf);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_seek_failed", 19);
    }
    /* read() on FreeBSD can return less than requested even for regular
     * files (e.g. when reaching a page-cache boundary mid-read). Loop
     * until we've filled `limit` bytes or hit EOF. */
    size_t got = 0;
    while (got < (size_t)limit) {
        r = read(fd, buf + got, (size_t)limit - got);
        if (r < 0) {
            if (errno == EINTR) continue;
            close(fd);
            free(buf);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_read_read_failed", 19);
        }
        if (r == 0) break;
        got += (size_t)r;
    }
    close(fd);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_READ_ACK, 0, trace_id,
                    (const char *)buf, (uint64_t)got);
    free(buf);
    return rc;
}

/* ── FS_COPY handler ─────────────────────────────────────────────────────
 *
 * Request JSON: {"from":"/abs/...","to":"/abs/..."}
 * ACK body:     empty (success); FTX2_FRAME_ERROR with a diagnostic tag
 *               on failure.
 *
 * Pre-checks: both paths pass is_path_allowed, `from` exists, `to` does
 * not. Cross-mount copies are fine (unlike FS_MOVE's rename()) because
 * cp_rf reads and writes bytes explicitly. That's the whole point of
 * having a separate FS_COPY frame — users need a way to move content
 * off the internal drive onto a USB or vice versa.
 */
static int handle_fs_copy(runtime_state_t *state, int client_fd,
                           uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char from[512], to[512];
    struct stat st;
    (void)body_len;
    if (!state) return -1;
    from[0] = '\0'; to[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "from", from, sizeof(from));
        extract_json_string_field(request_body, "to", to, sizeof(to));
    }
    if (!is_path_allowed(from) || !is_path_allowed(to)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_path_not_allowed", 24);
    }
    if (stat(from, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_source_missing", 22);
    }
    /* Refuse to clobber. The engine should have pre-checked via
     * FS_LIST_DIR; if we got here with dst present, it's a race or a
     * client bug — fail loudly. */
    struct stat dst_st;
    if (lstat(to, &dst_st) == 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_dest_exists", 19);
    }
    /* Register the in-flight op slot *before* walking so the engine's
     * 500 ms-cadence FS_OP_STATUS poller has somewhere to land its
     * first call. The walk is fast for few-large-file regimes
     * (PPSA01576/PPSA03977) but on small-file-heavy trees (PPSA01342:
     * 223k files / 19k dirs) it can outrun the poller's 250 ms initial
     * delay — and the old "walk then register" order let that race
     * surface as a parse error in the engine, which the client
     * mistook for "old payload" and showed a misleading banner. By
     * registering up front with total_bytes=0 and patching the total
     * in via fs_op_set_total once the walk finishes, the poller sees
     * a found:true op the moment it asks, with bytes_copied=0 and
     * total_bytes=0 ("scanning…") until the walk completes. Slot is
     * bounded (MAX_FS_OPS=4) — if all are full, fail this FS_COPY
     * rather than losing the ability to track or cancel it. */
    int op_idx = fs_op_register(trace_id, "fs_copy", from, to, 0);
    if (op_idx < 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_too_many_inflight", 25);
    }
    /* Pre-walk total bytes so the engine's progress poll can show a
     * percentage. Cheap (one stat per file) relative to the copy
     * itself, and a stat error means the copy will fail at the same
     * file shortly anyway — surface the issue early. Release the slot
     * on walk failure so a recoverable error doesn't burn one of the
     * MAX_FS_OPS=4 slots until process exit.
     *
     * `recursive_size_op` checks the cancel flag at directory
     * boundaries so a Stop click during the pre-walk takes effect
     * promptly. On a 200k-file tree the pre-walk can take seconds;
     * waiting for cp_rf to start before honoring cancel made the
     * Stop button feel unresponsive. */
    uint64_t total_bytes = 0;
    int walk_rc = recursive_size_op(from, &total_bytes, op_idx);
    if (walk_rc == -2) {
        fs_op_release(op_idx);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_cancelled", 17);
    }
    if (walk_rc != 0) {
        fs_op_release(op_idx);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_walk_failed", 19);
    }
    fs_op_set_total(op_idx, total_bytes);
    int copy_rc = cp_rf_op(from, to, 0, op_idx);
    fs_op_release(op_idx);
    if (copy_rc == -2) {
        /* Cancelled mid-flight. Recursively wipe whatever was
         * already copied so the next FS_COPY of the same source
         * isn't blocked by `fs_copy_dest_exists`. cp_rf_op already
         * unlinked the in-flight file; for the directory case we
         * also need to remove the partial tree we mkdir'd. Failure
         * here is non-fatal — the user can clean up manually if
         * the rm_rf doesn't catch everything (e.g. permissions). */
        (void)rm_rf(to, 0);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_cancelled", 17);
    }
    if (copy_rc != 0) {
        /* Hard error — same partial-state cleanup, same rationale.
         * The user shouldn't have to manually rm a half-copied
         * folder before retrying. */
        (void)rm_rf(to, 0);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_failed", 14);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_COPY_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MOUNT / FS_UNMOUNT helpers ───────────────────────────────────────
 *
 * MD-backend attach + nmount pipeline. Simplified to the subset we
 * need:
 *   - MD (memory-disk) attach only; no LVD, no PFS
 *   - exfatfs (.exfat) and ufs (.ffpkg) only; no PFS crypto
 *   - Single mount root (/mnt/ps5upload/) so it never collides
 *     with mount-points other utilities create.
 */

/* Fill `out` with an iovec entry for a NUL-terminated C string. The
 * iovec length MUST include the trailing NUL because nmount parses
 * key=value pairs via strings. Empty value (NULL) is allowed — used
 * for boolean flags like "async" / "noatime". */
static void fs_mount_iov(struct iovec *out, const char *s) {
    if (s == NULL) {
        out->iov_base = NULL;
        out->iov_len = 0;
    } else {
        out->iov_base = (void *)s;
        out->iov_len = strlen(s) + 1;
    }
}

/* Wait for a device node to exist (or disappear, when exist=0).
 * Returns 0 on success, -1 on timeout. */
static int fs_mount_wait_node(const char *devname, int exist) {
    struct stat st;
    int i;
    for (i = 0; i < FS_MOUNT_DEV_WAIT_RETRIES; i++) {
        int got = (stat(devname, &st) == 0);
        if (exist ? got : !got) return 0;
        usleep(FS_MOUNT_DEV_WAIT_US);
    }
    return -1;
}

/* Filename test: lowercase extension match for .exfat / .ffpkg /
 * .ffpfs. Returns the FreeBSD fstype name nmount expects. */
static int fs_mount_detect_fstype(const char *image_path, char *fstype_out, size_t cap) {
    size_t len = strlen(image_path);
    if (len < 6) return -1;
    const char *ext = image_path + len;
    while (ext > image_path && *(ext - 1) != '.') ext--;
    if (ext == image_path || *(ext - 1) != '.') return -1;
    char buf[16];
    size_t el = strlen(ext);
    if (el >= sizeof(buf)) return -1;
    size_t j;
    for (j = 0; j < el; j++) {
        char c = ext[j];
        buf[j] = (c >= 'A' && c <= 'Z') ? (char)(c - 'A' + 'a') : c;
    }
    buf[el] = '\0';
    if (strcmp(buf, "exfat") == 0) {
        snprintf(fstype_out, cap, "exfatfs");
        return 0;
    }
    if (strcmp(buf, "ffpkg") == 0) {
        snprintf(fstype_out, cap, "ufs");
        return 0;
    }
    if (strcmp(buf, "ffpfs") == 0) {
        snprintf(fstype_out, cap, "pfs");
        return 0;
    }
    return -1;
}

/* Derive a filesystem-safe mount name from the image basename:
 * strip directory prefix, strip the trailing .exfat/.ffpkg extension,
 * replace anything outside [A-Za-z0-9_.-] with '_'. Output is written
 * to `out` with a guaranteed NUL terminator.
 *
 * Examples:
 *   /data/homebrew/Foo Bar.exfat  → "Foo_Bar"
 *   /mnt/ext1/My-Game.ffpkg       → "My-Game"
 */
static void fs_mount_derive_name(const char *image_path, char *out, size_t cap) {
    if (cap == 0) return;
    if (image_path == NULL) {
        /* Defensive — extract_json_string_field always writes a NUL
         * before returning, so live callers never pass NULL. Keep the
         * guard so a future refactor that skips the extract can't
         * crash us with a NULL deref inside strrchr. */
        snprintf(out, cap, "image");
        return;
    }
    const char *base = strrchr(image_path, '/');
    base = base ? base + 1 : image_path;
    size_t len = strlen(base);
    /* Strip extension. */
    const char *dot = strrchr(base, '.');
    if (dot != NULL && dot > base) len = (size_t)(dot - base);
    if (len >= cap) len = cap - 1;
    size_t i;
    for (i = 0; i < len; i++) {
        char c = base[i];
        int ok = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
                 (c >= '0' && c <= '9') || c == '_' || c == '-' || c == '.';
        out[i] = ok ? c : '_';
    }
    out[len] = '\0';
    if (len == 0) snprintf(out, cap, "image");
}

/* Multi-pass unmount. PS5 mount stacks can have a layer (e.g. a
 * nullfs over a UFS image) where unmounting the top exposes a
 * second mount of the same fspath that needs another unmount call.
 * The single try-then-force form would leave that residual layer
 * attached and the next FS_LIST_VOLUMES would still report the
 * fspath as mounted. Loop up to FS_MOUNT_UNMOUNT_PASSES, exiting
 * early when the path is no longer a mount point or no progress
 * was made.
 *
 * "No progress" means: this iteration neither succeeded at
 * unmounting nor saw the path go non-mounted via statfs. Without
 * the progress check, a stuck-busy mount would burn the full pass
 * budget on every call. Used by both FS_UNMOUNT and the
 * error-cleanup path of FS_MOUNT. Returns 0 on success, -1 on
 * failure (errno preserved). */
#define FS_MOUNT_UNMOUNT_PASSES 4
static int fs_mount_try_unmount(const char *mount_point) {
    int last_errno = 0;
    for (int pass = 0; pass < FS_MOUNT_UNMOUNT_PASSES; pass++) {
        /* Plain unmount first. EINVAL / ENOENT ⇒ "already gone" —
         * treat as success on the first pass; on later passes it
         * means a previous pass cleared the last layer. */
        if (unmount(mount_point, 0) == 0) continue;
        last_errno = errno;
        if (errno == EINVAL || errno == ENOENT) {
            errno = 0;
            return 0;
        }
        /* Busy → escalate to MNT_FORCE for this pass. If the
         * forced unmount also fails, bail — one more pass with the
         * same input would just repeat the failure. */
        if (unmount(mount_point, MNT_FORCE) == 0) continue;
        last_errno = errno;
        if (errno == EINVAL || errno == ENOENT) {
            errno = 0;
            return 0;
        }
        errno = last_errno;
        return -1;
    }
    /* All passes consumed and the path is still busy. The most
     * common cause is a process holding an open fd or cwd inside
     * the mount. Surface the last errno so the caller can include
     * it in the user-visible error frame. */
    errno = last_errno;
    return -1;
}

/* Detach an MD unit. Tries plain detach, then MD_FORCE if that fails
 * with EBUSY — leaving an attached unit orphans a kernel resource
 * until reboot, so we lean on force after a polite attempt.
 *
 * After a successful detach we wait for `/dev/md<N>` to disappear so
 * the next attach round won't race the kernel teardown and reuse a
 * unit number whose vnode hasn't actually been released yet.
 * Without this, a fast remount cycle can fail with EBUSY on the
 * new attach because the old node is still in `getmntinfo`'s
 * view. */
static int fs_mount_detach_md(int unit_id) {
    if (unit_id < 0) return 0;
    int fd = open(FS_MOUNT_MD_CTL, O_RDWR);
    if (fd < 0) return -1;
    struct md_ioctl req;
    memset(&req, 0, sizeof(req));
    req.md_version = MDIOVERSION;
    req.md_unit = (unsigned int)unit_id;
    int rc = ioctl(fd, MDIOCDETACH, &req);
    if (rc != 0) {
        req.md_options = MD_FORCE;
        rc = ioctl(fd, MDIOCDETACH, &req);
    }
    close(fd);
    if (rc != 0) return -1;
    char devname[32];
    snprintf(devname, sizeof(devname), "/dev/md%d", unit_id);
    /* Best-effort: a node still present after the wait window doesn't
     * make this detach call "fail" — the ioctl returned 0 and the
     * kernel will eventually finish teardown — but a caller that
     * immediately reattaches at the same unit may still hit a stale
     * node. Surface that as "detach succeeded but watch out" by
     * returning 0; the rare case where it actually matters is the
     * fs_mount retry path, which uses auto-assign and won't pick the
     * same unit. */
    (void)fs_mount_wait_node(devname, 0);
    return 0;
}

/* Detach an LVD unit. No force variant on LVD — best effort. Same
 * post-detach node-wait rationale as fs_mount_detach_md. */
static int fs_mount_detach_lvd(int unit_id) {
    if (unit_id < 0) return 0;
    int fd = open(FS_MOUNT_LVD_CTL, O_RDWR);
    if (fd < 0) return -1;
    fs_mount_lvd_detach_t req;
    memset(&req, 0, sizeof(req));
    req.device_id = unit_id;
    int rc = ioctl(fd, FS_MOUNT_LVD_IOC_DETACH, &req);
    close(fd);
    if (rc != 0) return -1;
    char devname[32];
    snprintf(devname, sizeof(devname), "/dev/lvd%d", unit_id);
    (void)fs_mount_wait_node(devname, 0);
    return 0;
}

/* fs_mount_kind_t — local enum capturing which (fstype, secondary_unit,
 * image_type) triple we want for an attach. Cheap to pass around and
 * makes the call sites readable without hauling around bare strings. */
typedef enum {
    FS_MOUNT_KIND_EXFAT = 0,
    FS_MOUNT_KIND_UFS   = 1,
    FS_MOUNT_KIND_PFS   = 2,
} fs_mount_kind_t;

static fs_mount_kind_t fs_mount_kind_from_fstype(const char *fstype) {
    if (strcmp(fstype, "exfatfs") == 0) return FS_MOUNT_KIND_EXFAT;
    if (strcmp(fstype, "pfs")     == 0) return FS_MOUNT_KIND_PFS;
    return FS_MOUNT_KIND_UFS;  /* "ufs" */
}

/* Default device sector sizes per fstype. exfat lives happily on
 * 512 because exFAT metadata is sector-granular; UFS-DD and PFS
 * use 4096-byte blocks. The "default" qualifier matters because
 * some images on small-cluster host filesystems may need a
 * smaller value — see post-mount sector validation. */
static uint32_t fs_mount_default_sector(fs_mount_kind_t kind) {
    if (kind == FS_MOUNT_KIND_EXFAT) return 512u;
    return 4096u;  /* UFS / PFS */
}

/* Attach a disk image via the Sony LVD driver. Returns assigned unit
 * id on success, or -1 with errno set on failure. `kind` selects the
 * sector/flags/image_type triple, `read_only` swaps RW for RO LVD
 * flag presets. On PS5, this is the primary path: MDIOCATTACH often
 * returns EPERM or EINVAL on PS5 where LVD succeeds, because PS5
 * routes file-backed block devices through its own virtualized
 * layer.
 *
 * The V0 attach ioctl takes a single layer descriptor pointing at
 * the user-space path. The kernel opens the file itself inside the
 * ioctl handler, so we don't need to keep an open fd around. */
static int fs_mount_attach_lvd(const char *image_path, off_t size,
                                fs_mount_kind_t kind, int read_only) {
    int fd = open(FS_MOUNT_LVD_CTL, O_RDWR);
    if (fd < 0) return -1;

    fs_mount_lvd_layer_t layer;
    memset(&layer, 0, sizeof(layer));
    layer.source_type = 1;                 /* LVD_ENTRY_TYPE_FILE */
    layer.flags       = 0x1;               /* LVD_ENTRY_FLAG_NO_BITMAP */
    layer.path        = image_path;
    layer.offset      = 0;
    layer.size        = (uint64_t)size;

    uint32_t sector_size = fs_mount_default_sector(kind);
    uint32_t secondary_unit;
    uint16_t flags;
    uint16_t image_type;
    switch (kind) {
        case FS_MOUNT_KIND_EXFAT:
            secondary_unit = FS_MOUNT_LVD_SECONDARY_SINGLE;
            flags          = read_only ? FS_MOUNT_LVD_FLAGS_EXFAT_RO
                                       : FS_MOUNT_LVD_FLAGS_EXFAT_RW;
            image_type     = FS_MOUNT_LVD_IMAGE_SINGLE;
            break;
        case FS_MOUNT_KIND_PFS:
            /* PFS uses the SINGLE family for layer geometry but a
             * different image_type so the kernel routes the mount
             * through devpfs. RW raw 0x8 normalizes to 0x14 for
             * PFS as it does for exfat. */
            secondary_unit = sector_size;
            flags          = read_only ? FS_MOUNT_LVD_FLAGS_PFS_RO
                                       : FS_MOUNT_LVD_FLAGS_PFS_RW;
            image_type     = FS_MOUNT_LVD_IMAGE_PFS_SAVE;
            break;
        case FS_MOUNT_KIND_UFS:
        default:
            secondary_unit = sector_size;
            flags          = read_only ? FS_MOUNT_LVD_FLAGS_UFS_RO
                                       : FS_MOUNT_LVD_FLAGS_UFS_RW;
            image_type     = FS_MOUNT_LVD_IMAGE_UFS_DD;
            break;
    }

    fs_mount_lvd_attach_t req;
    memset(&req, 0, sizeof(req));
    req.io_version     = 0;                          /* V0 */
    req.device_id      = -1;                         /* auto-assign */
    req.sector_size    = sector_size;
    req.secondary_unit = secondary_unit;
    req.flags          = flags;
    req.image_type     = image_type;
    req.layer_count    = 1;
    req.device_size    = (uint64_t)size;
    req.layers_ptr     = &layer;

    int rc = ioctl(fd, FS_MOUNT_LVD_IOC_ATTACH_V0, &req);
    int saved_errno = errno;
    close(fd);
    if (rc != 0) {
        errno = saved_errno;
        return -1;
    }
    /* Defensive: the validator can return rc=0 with device_id=-1 on
     * some firmware. Treat that as an attach failure so we don't
     * try to wait for /dev/lvd-1. */
    if (req.device_id < 0) {
        errno = EINVAL;
        return -1;
    }
    return req.device_id;
}

/* Attach via the plain FreeBSD memory-disk driver. Fallback path used
 * when LVD is unavailable or refuses the attach. Returns assigned
 * unit id on success, or -1 with errno set on failure. */
static int fs_mount_attach_md(const char *image_path, off_t size,
                               fs_mount_kind_t kind, int read_only) {
    /* MD uses the same 512-byte sector size for every fstype on PS5,
     * so `kind` is informational only today. PFS via MD is not
     * supported by Sony's devpfs — gate it out. */
    (void)kind;
    if (kind == FS_MOUNT_KIND_PFS) {
        errno = ENOTSUP;
        return -1;
    }
    int fd = open(FS_MOUNT_MD_CTL, O_RDWR);
    if (fd < 0) return -1;

    struct md_ioctl req;
    memset(&req, 0, sizeof(req));
    req.md_version    = MDIOVERSION;
    req.md_type       = MD_VNODE;
    req.md_file       = (char *)image_path;
    req.md_mediasize  = size;
    /* PS5 images use 512-byte sectors for both exfat and ufs — 4096
     * fails MDIOCATTACH with EINVAL on most firmware. */
    req.md_sectorsize = 512;
    req.md_options    = MD_AUTOUNIT | MD_ASYNC;
    if (read_only) req.md_options |= MD_READONLY;

    int rc = ioctl(fd, MDIOCATTACH, &req);
    int saved_errno = errno;
    close(fd);
    if (rc != 0) {
        errno = saved_errno;
        return -1;
    }
    return (int)req.md_unit;
}

/* mkdir -p equivalent: creates each parent directory if missing,
 * then the target itself. Stops at the first segment that fails for
 * a reason other than EEXIST. Used by FS_MOUNT when the caller
 * picks a deep mount path like /mnt/ext1/games/foo and the
 * intermediate directory might not exist yet. mode 0777 matches
 * the existing single-mkdir call; PS5 uses no umask of consequence
 * for our case (the kernel sandbox limits visibility long before
 * permissions matter). */
static int fs_mount_mkdir_p(const char *path) {
    if (!path || path[0] != '/') return -1;
    char buf[256];
    size_t len = strlen(path);
    if (len + 1 > sizeof(buf)) return -1;
    memcpy(buf, path, len + 1);
    /* Walk the path, terminating at each '/' to mkdir intermediates. */
    for (size_t i = 1; i < len; i++) {
        if (buf[i] != '/') continue;
        buf[i] = '\0';
        if (mkdir(buf, 0777) != 0 && errno != EEXIST) {
            buf[i] = '/';
            return -1;
        }
        buf[i] = '/';
    }
    if (mkdir(buf, 0777) != 0 && errno != EEXIST) return -1;
    return 0;
}

/* Find an existing mount whose source-tracker file points at the same
 * image_path the caller is asking about. Returns 1 with `out_mp`
 * filled (NUL-terminated, capped at out_cap) if found, 0 otherwise.
 *
 * Used by handle_fs_mount to short-circuit a re-mount: a user
 * double-clicking Mount, an upload-then-mount flow racing the
 * Library refresh, or a post-takeover client that doesn't know
 * the previous payload session already mounted the image — all of
 * these used to allocate a new LVD slot per click and either
 * pile up failed attaches or mask the existing mount with a new
 * overlay at the same target. Returning the existing mount_point
 * is harmless. */
static int fs_mount_find_existing(const char *image_path,
                                   char *out_mp, size_t out_cap) {
    if (!image_path || !*image_path || !out_mp || out_cap == 0) return 0;
    out_mp[0] = '\0';
    struct statfs *mnts = NULL;
    int nmnts = mntinfo_snapshot(&mnts);
    if (nmnts <= 0 || !mnts) return 0;
    for (int i = 0; i < nmnts; i++) {
        const char *mnt_on   = mnts[i].f_mntonname;
        const char *mnt_from = mnts[i].f_mntfromname;
        /* Only consider mounts backed by a virtual block device the
         * payload would have created (or could have inherited from a
         * previous payload session). System nullfs mounts and
         * Sony-managed mounts have different prefixes. */
        const int is_lvd = (strncmp(mnt_from, "/dev/lvd", 8) == 0);
        const int is_md  = (strncmp(mnt_from, "/dev/md",  7) == 0);
        if (!is_lvd && !is_md) continue;
        char src[512];
        if (!mount_tracker_read(mnt_on, src, sizeof(src))) continue;
        if (strcmp(src, image_path) != 0) continue;
        /* Found one. Surface it to the caller. */
        size_t len = strlen(mnt_on);
        if (len + 1 > out_cap) { free(mnts); return 0; }
        memcpy(out_mp, mnt_on, len + 1);
        free(mnts);
        return 1;
    }
    free(mnts);
    return 0;
}

/* Post-mount sanity check. After nmount succeeds, the mount actually
 * needs to be readable for the user — and the kernel will happily
 * mount a UFS image whose cluster size is smaller than the sector
 * size we attached at, which produces a half-broken mount point that
 * EIOs on every read.
 *
 * Returns 0 on ok, -1 on failure with errbuf populated.
 *
 * We deliberately don't try to autotune (write a per-image override
 * file, retry with smaller sector). Autotuning would require an
 * asynchronous retry path that doesn't translate to a synchronous
 * user-driven Mount click. Surfacing the error with the f_bsize
 * value lets the user pick a proper image_sector hint or remake
 * the image with a larger cluster. */
static int fs_mount_validate_post_mount(const char *mp,
                                         fs_mount_kind_t kind,
                                         const char *expected_dev,
                                         char *errbuf, size_t errbuf_cap) {
    if (!mp || !errbuf || errbuf_cap == 0) {
        if (errbuf && errbuf_cap > 0) errbuf[0] = '\0';
        return -1;
    }
    errbuf[0] = '\0';
    struct statfs sfs;
    if (statfs(mp, &sfs) != 0) {
        snprintf(errbuf, errbuf_cap,
                 "fs_mount_post_statfs_failed: %s", strerror(errno));
        return -1;
    }

    /* nmount(2) returning 0 doesn't always mean the kernel actually
     * attached the filesystem at this path — on some firmware quirks
     * + cross-volume mount-policy refusals it returns success but
     * the mount table still shows the *parent* fs at the path. The
     * symptom users hit: "mount succeeded but I see nothing at the
     * mount point, and Library refresh shows no games inside."
     * Verify by reading f_mntfromname and confirming it matches our
     * just-attached /dev/lvdN or /dev/mdN. If it doesn't, the mount
     * silently fell through and we should surface that as an error
     * rather than write a tracker that pretends it worked. */
    if (expected_dev && expected_dev[0]) {
        /* Use strcmp, not strncmp(...,sizeof(f_mntfromname)). Both
         * fields are NUL-terminated short strings (~10 bytes for
         * /dev/lvdN); strncmp's length cap was a redundant guard
         * that just made prefix collisions theoretically possible
         * if expected_dev ever exceeded MNAMELEN. strcmp is the
         * correct full-string comparison. */
        if (strcmp(sfs.f_mntfromname, expected_dev) != 0) {
            snprintf(errbuf, errbuf_cap,
                     "fs_mount_silent_failure: nmount returned 0 but the kernel "
                     "mount table at %s still shows %s — expected %s. The .ffpkg "
                     "wasn't actually attached. Try a different mount point under "
                     "/data/ or /mnt/ps5upload/ — kernel mount-policy may be "
                     "refusing this path.",
                     mp, sfs.f_mntfromname, expected_dev);
            return -1;
        }
    }

    uint32_t min_sector = fs_mount_default_sector(kind);
    uint64_t bsize = (uint64_t)sfs.f_bsize;
    if (bsize == 0) bsize = (uint64_t)sfs.f_iosize;
    if (bsize == 0) {
        /* Can't validate without a block size. Prefer "succeed" over
         * "reject a working mount on a kernel quirk" — the alternative
         * is a Mount that always fails on devices statfs reports
         * zeros for. */
        return 0;
    }
    if (bsize < (uint64_t)min_sector) {
        snprintf(errbuf, errbuf_cap,
                 "fs_mount_cluster_too_small: f_bsize=%llu < sector=%u "
                 "— remake the image with a larger cluster",
                 (unsigned long long)bsize, (unsigned)min_sector);
        return -1;
    }
    return 0;
}

/* ── FS_MOUNT handler ───────────────────────────────────────────────────
 *
 * Pipeline:
 *   1. Parse {image_path, mount_name?, mount_point?, read_only?} from
 *      JSON body.
 *   2. Verify image_path passes is_path_allowed and names a regular
 *      file. Detect fstype by extension (.exfat → exfatfs,
 *      .ffpkg → ufs, .ffpfs → pfs).
 *   3. Source-stability gate: refuse if mtime is too fresh (avoid
 *      mounting an in-progress upload).
 *   4. Reuse-existing-mount: if the same image_path is already
 *      mounted, ACK with that mount_point unchanged.
 *   5. Resolve mount_point. Three cases, in priority:
 *        - Caller-supplied `mount_point`: full absolute path. Must
 *          pass is_path_allowed; rejected otherwise.
 *        - Caller-supplied `mount_name` (no slashes): mounts under
 *          /mnt/ps5upload/<name>/ (the legacy 2.2.24 path).
 *        - Neither: derive a safe name from image_path basename and
 *          mount under /mnt/ps5upload/.
 *   6. LVD attach (PS5-native); MD fallback. Wait for the device
 *      node to materialize.
 *   7. mkdir -p mount point; nmount with the per-fstype iovec set
 *      and the per-fstype third-arg flag (UFS magic for .ffpkg).
 *   8. Post-mount validate: f_bsize must be at least the device
 *      sector size, otherwise the mount is silently broken.
 *   9. On any failure after attach, detach the LVD/MD unit so we
 *      don't leak slots.
 *  10. Reply with ACK carrying {mount_point, dev_node, fstype,
 *      source_image, read_only}.
 */
static int handle_fs_mount(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body,
                            uint64_t body_len) {
    char image_path[512] = {0};
    char mount_name[128] = {0};
    char fstype[16] = {0};
    char mount_point[256] = {0};
    char devname[32] = {0};
    /* ACK body holds JSON with up to-512-char escaped image_path, up to
     * 256-char escaped mount_point, plus dev/fstype/source_image and the
     * 2.2.52 diagnostic fields (f_bsize/f_iosize/kernel_ro). Worst-case
     * with realistic inputs (300-char image_path + 200-char mount_point)
     * was ~980 bytes, the prior resp[768] silently truncated and
     * snprintf set n=0 → engine received a successful FS_MOUNT_ACK with
     * an empty body, losing the resolved mount_point. 2 KiB has headroom
     * for the worst-case escaped-path lengths. */
    char resp[2048];
    char mount_errmsg[256] = {0};
    struct stat st;
    int unit_id = -1;
    int read_only = 0;
    int n;
    (void)body_len;
    if (!state) return -1;

    if (request_body) {
        extract_json_string_field(request_body, "image_path", image_path, sizeof(image_path));
        extract_json_string_field(request_body, "mount_name", mount_name, sizeof(mount_name));
        extract_json_string_field(request_body, "mount_point", mount_point, sizeof(mount_point));
        /* read_only is a JSON number (0 or 1). 1, 2, … all mean "RO";
         * 0 or absent means "RW" (current default). Using a number
         * instead of a JSON true/false keeps extract_json_uint64_field
         * happy and avoids adding a bool parser. */
        read_only = (extract_json_uint64_field(request_body, "read_only") != 0) ? 1 : 0;
    }
    if (!is_path_allowed(image_path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_path_not_allowed", 25);
    }
    if (stat(image_path, &st) != 0 || !S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_image_not_a_file", 25);
    }
    if (fs_mount_detect_fstype(image_path, fstype, sizeof(fstype)) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_unsupported_format", 27);
    }

    /* Source-stability gate. mtime in the future or in a kernel that
     * reports st_mtime=0 (rare) shouldn't block — only reject when we
     * can prove the file is actively being written to. Using time(NULL)
     * matches FreeBSD wallclock; the few seconds of skew that NTP
     * could introduce don't matter for a 3-second guard. */
    {
        time_t now = time(NULL);
        if (st.st_mtime > 0 && now > st.st_mtime &&
            (now - st.st_mtime) < FS_MOUNT_STABILITY_SECONDS) {
            char errbuf[160];
            int el = snprintf(errbuf, sizeof(errbuf),
                              "fs_mount_source_unstable: image modified %lld s ago "
                              "(<%d s); wait for the upload to settle",
                              (long long)(now - st.st_mtime),
                              FS_MOUNT_STABILITY_SECONDS);
            if (el < 0) el = 0;
            if ((size_t)el >= sizeof(errbuf)) el = (int)sizeof(errbuf) - 1;
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              errbuf, (uint64_t)el);
        }
    }

    /* Reuse-existing-mount short-circuit. If this exact image_path is
     * already mounted from an earlier FS_MOUNT call (or inherited
     * from a prior payload session), return that mount_point as
     * ACK without touching LVD. Saves a slot and avoids the silent
     * overlay-mount footgun (mounting on top of an existing mount
     * leaves the original behind, invisible). */
    {
        char existing[256];
        if (fs_mount_find_existing(image_path, existing, sizeof(existing))) {
            /* Synthesize a dev_node string by looking up the
             * f_mntfromname for that mount. Best-effort — if we can't
             * find it the ACK still carries the mount_point. */
            char existing_dev[32] = "";
            struct statfs *mnts = NULL;
            int nmnts = mntinfo_snapshot(&mnts);
            for (int i = 0; i < nmnts && mnts; i++) {
                if (strcmp(mnts[i].f_mntonname, existing) != 0) continue;
                snprintf(existing_dev, sizeof(existing_dev), "%s",
                         mnts[i].f_mntfromname);
                break;
            }
            free(mnts);
            /* Re-write the source tracker on reuse — idempotent. If the
             * tracker file got deleted out-of-band (manual cleanup,
             * orphan-reconciliation race, etc.) the next FS_UNMOUNT
             * would refuse with `fs_unmount_not_our_mount` because
             * `mount_tracker_exists` returns 0; the user would see a
             * mount they can't tear down. Writing on every reuse
             * heals that state without changing the legitimate-reuse
             * path. */
            mount_tracker_write(existing, image_path);
            /* JSON-escape every user-controllable string. is_path_allowed
             * accepts paths containing '"' or '\' (only `..` and a few
             * shapes are rejected) so a path like /data/foo"bar would
             * unescape into invalid JSON in the ACK. Pre-2.2.52 these
             * sites embedded the raw chars and broke the engine's
             * decoder for any path containing a quote or backslash. */
            char ex_esc[768], exdev_esc[64], img_esc[1024], fs_esc[32];
            json_escape_into(existing,     ex_esc,    sizeof(ex_esc));
            json_escape_into(existing_dev, exdev_esc, sizeof(exdev_esc));
            json_escape_into(image_path,   img_esc,   sizeof(img_esc));
            json_escape_into(fstype,       fs_esc,    sizeof(fs_esc));
            /* Emit bool fields as JSON booleans (true/false), not as
             * integer 0/1. The engine's MountResult.read_only is
             * declared as `bool` and serde rejects integer-for-bool
             * with `invalid type: integer 1, expected a boolean` —
             * which used to fail every fs_mount on first attempt with
             * "decode FS_MOUNT_ACK body as JSON" (see engine.log).
             * Same fix as round 1's PKG_INSTALL_ACK bool fix. */
            const char *ro_str = read_only ? "true" : "false";
            n = snprintf(resp, sizeof(resp),
                         "{\"mount_point\":\"%s\",\"dev_node\":\"%s\","
                         "\"fstype\":\"%s\",\"source_image\":\"%s\","
                         "\"read_only\":%s,\"reused\":true}",
                         ex_esc, exdev_esc, fs_esc, img_esc, ro_str);
            if (n < 0 || (size_t)n >= sizeof(resp)) n = 0;
            return send_frame(client_fd, FTX2_FRAME_FS_MOUNT_ACK, 0,
                              trace_id, resp, (uint64_t)n);
        }
    }

    /* Resolve mount_point. Caller-supplied path takes precedence — if
     * provided, the leaf name and base dir are determined by the
     * caller, and is_path_allowed enforces the same writable-roots
     * allowlist used by every other FS-mutation frame. Reject the
     * /mnt/ps5upload base itself (the existing namespace would leak
     * into a "mount point at the namespace root" footgun) and reject
     * paths with a trailing slash so unmount's exact-string match
     * stays well-defined. */
    if (mount_point[0] != '\0') {
        if (!is_path_allowed(mount_point)) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_mount_path_not_allowed", 25);
        }
        size_t mp_len = strlen(mount_point);
        if (mp_len > 1 && mount_point[mp_len - 1] == '/') {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_mount_bad_mount_point", 24);
        }
        if (strcmp(mount_point, FS_MOUNT_BASE) == 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_mount_bad_mount_point", 24);
        }
    } else {
        /* No mount_point supplied — fall back to legacy
         * /mnt/ps5upload/<name> behavior. mount_name is either
         * caller-supplied (validated against slashes/..) or derived
         * from the image filename. */
        if (mount_name[0] == '\0') {
            fs_mount_derive_name(image_path, mount_name, sizeof(mount_name));
        }
        /* Validate whether the name was caller-supplied or just derived
         * from the image filename: reject path separators, "..", and a
         * lone ".". The "." case matters because snprintf below then
         * builds /mnt/ps5upload/. which the kernel normalises to
         * FS_MOUNT_BASE itself — shadowing every existing mount in the
         * namespace. That is the same footgun the strcmp(mount_point,
         * FS_MOUNT_BASE) guard blocks for caller-supplied mount_points
         * above, but this derived path bypasses it. fs_mount_derive_name
         * can also yield "." for image names like "..exfat" or ".". */
        if (strchr(mount_name, '/') != NULL ||
            strstr(mount_name, "..") != NULL ||
            strcmp(mount_name, ".") == 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_mount_bad_name", 17);
        }
        n = snprintf(mount_point, sizeof(mount_point), "%s/%s",
                     FS_MOUNT_BASE, mount_name);
        if (n < 0 || (size_t)n >= sizeof(mount_point)) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_mount_name_too_long", 22);
        }
    }

    /* Attach pipeline: try LVD first (PS5-native, works on more
     * firmware), fall back to MD if LVD returns an error. Each
     * failure records errno verbatim so clients can surface the
     * actual reason — "fs_mount_attach_failed" alone is useless for
     * diagnosis. The composite error names every attempt. */
    const fs_mount_kind_t kind = fs_mount_kind_from_fstype(fstype);
    int lvd_unit = fs_mount_attach_lvd(image_path, st.st_size, kind, read_only);
    int lvd_errno = errno;
    int used_lvd = 0;
    if (lvd_unit >= 0) {
        used_lvd = 1;
        unit_id = lvd_unit;
        snprintf(devname, sizeof(devname), "/dev/lvd%d", unit_id);
    } else {
        int md_unit = fs_mount_attach_md(image_path, st.st_size, kind, read_only);
        int md_errno = errno;
        if (md_unit < 0) {
            /* Both backends failed — surface both errnos so the user
             * (or a log trawl) can tell whether LVD refused a policy
             * check or MD refused a sector-size, etc. */
            char errbuf[256];
            int el = snprintf(errbuf, sizeof(errbuf),
                              "fs_mount_attach_failed: lvd=%s md=%s",
                              strerror(lvd_errno), strerror(md_errno));
            if (el < 0) el = 0;
            if ((size_t)el >= sizeof(errbuf)) el = (int)sizeof(errbuf) - 1;
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              errbuf, (uint64_t)el);
        }
        unit_id = md_unit;
        snprintf(devname, sizeof(devname), "/dev/md%d", unit_id);
    }
    if (fs_mount_wait_node(devname, 1) != 0) {
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_dev_node_missing", 25);
    }

    /* Ensure every directory along the mount_point exists. The
     * legacy /mnt/ps5upload/<name> path needs at most two mkdirs
     * (the base + the leaf); a user-chosen path like
     * /mnt/ext1/games/foo needs three or more. mkdir -p handles
     * both. EEXIST at any segment is fine — we'll nmount over
     * whatever's there. */
    if (fs_mount_mkdir_p(mount_point) != 0) {
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_mkdir_failed", 21);
    }

    /* Build nmount iovec. Keys + ordering chosen to match what the
     * PS5 kernel accepts across firmware revisions — experimenting
     * with these on hardware tends to break mounts. "budgetid=game"
     * is the PS5-specific resource class for user-installed titles.
     *
     * PFS adds a fistful of crypto/playgo/disc options; the
     * zero-EKPFS key + sigverify=0 path works for fake-signed
     * images on the firmwares we target.
     *
     * Slot budget (worst case is the PFS branch):
     *   fstype/from/fspath/budgetid          = 8
     *   sigverify/mkeymode/playgo/disc/ekpfs = 10 (pfs only)
     *   large/timezone/ignoreacl             = 6 (exfat only)
     *   async/noatime/automounted            = 6
     *   errmsg                               = 2
     * = 32 slots. Array sized to 40 for headroom. */
    struct iovec iov[40];
    int iovlen = 0;
    #define FS_MOUNT_PUSH(k, v) do { \
        fs_mount_iov(&iov[iovlen++], (k)); \
        fs_mount_iov(&iov[iovlen++], (v)); \
    } while (0)
    FS_MOUNT_PUSH("fstype", fstype);
    FS_MOUNT_PUSH("from", devname);
    FS_MOUNT_PUSH("fspath", mount_point);
    FS_MOUNT_PUSH("budgetid", "game");
    if (kind == FS_MOUNT_KIND_EXFAT) {
        FS_MOUNT_PUSH("large", "yes");
        FS_MOUNT_PUSH("timezone", "static");
        FS_MOUNT_PUSH("ignoreacl", NULL);
    } else if (kind == FS_MOUNT_KIND_PFS) {
        FS_MOUNT_PUSH("sigverify", FS_MOUNT_PFS_SIGVERIFY);
        FS_MOUNT_PUSH("mkeymode",  FS_MOUNT_PFS_MKEYMODE);
        FS_MOUNT_PUSH("playgo",    FS_MOUNT_PFS_PLAYGO);
        FS_MOUNT_PUSH("disc",      FS_MOUNT_PFS_DISC);
        FS_MOUNT_PUSH("ekpfs",     FS_MOUNT_PFS_EKPFS_HEX);
    }
    FS_MOUNT_PUSH("async", NULL);
    FS_MOUNT_PUSH("noatime", NULL);
    FS_MOUNT_PUSH("automounted", NULL);
    /* errmsg buffer — nmount writes the kernel-side error string here
     * on failure. Surface it verbatim to the client so diagnostics
     * survive the wire trip. */
    fs_mount_iov(&iov[iovlen++], "errmsg");
    iov[iovlen].iov_base = mount_errmsg;
    iov[iovlen].iov_len = sizeof(mount_errmsg);
    iovlen++;
    #undef FS_MOUNT_PUSH

    /* Per-fstype nmount flags. UFS images need the 0x10000000 magic
     * (RW) or 0x10000001 (RO); exfatfs and pfs take MNT_RDONLY for RO
     * and 0 for RW. */
    unsigned int nmount_flags;
    if (kind == FS_MOUNT_KIND_UFS) {
        nmount_flags = read_only ? FS_MOUNT_UFS_NMOUNT_FLAG_RO
                                 : FS_MOUNT_UFS_NMOUNT_FLAG_RW;
    } else {
        nmount_flags = read_only ? (unsigned int)MNT_RDONLY : 0u;
    }
    if (nmount(iov, (unsigned)iovlen, (int)nmount_flags) != 0) {
        char errbuf[320];
        int len = snprintf(errbuf, sizeof(errbuf),
                           "fs_mount_nmount_failed: %s",
                           mount_errmsg[0] ? mount_errmsg : strerror(errno));
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        /* Clean up the empty mount dir so repeated attempts don't pile
         * up stale /mnt/ps5upload/<name> directories. */
        rmdir(mount_point);
        if (len < 0) len = 0;
        if ((size_t)len >= sizeof(errbuf)) len = (int)sizeof(errbuf) - 1;
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          errbuf, (uint64_t)len);
    }

    /* Post-mount sanity check. nmount succeeding doesn't guarantee
     * the mount is actually usable — a UFS image whose cluster size
     * is smaller than the LVD sector size mounts but EIOs on every
     * read. Reject + tear down so the user gets a clear actionable
     * error instead of a silent half-broken mount. */
    {
        char post_err[224];
        if (fs_mount_validate_post_mount(mount_point, kind, devname,
                                          post_err, sizeof(post_err)) != 0) {
            (void)fs_mount_try_unmount(mount_point);
            if (used_lvd) fs_mount_detach_lvd(unit_id);
            else fs_mount_detach_md(unit_id);
            rmdir(mount_point);
            size_t el = strlen(post_err);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              post_err, (uint64_t)el);
        }
    }

    /* Record the source image path so Volumes can surface it and
     * reconciliation can validate on next boot. Keyed by the
     * resolved mount_point (works for both legacy
     * /mnt/ps5upload/<name> and user-chosen paths via
     * mount_tracker_key). Best-effort — a failed tracker write
     * doesn't undo the successful mount. */
    mount_tracker_write(mount_point, image_path);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    /* Image layout pre-flight: verify <mount_point>/sce_sys/param.json
     * exists. If the user built the image with an extra top-level
     * folder (so files live at `<mount>/MyGame/sce_sys/...` instead
     * of `<mount>/sce_sys/...`), the mount succeeds but Register +
     * Launch will fail with a confusing error. Surface a `layout_valid`
     * flag to the host so the UI can warn the user before they try
     * to register.
     *
     * This is a stat() of the most predictable path. We don't walk
     * the directory looking for nested layouts because:
     *   1. The convention is documented (param.json at root) — any
     *      other layout is the user's bug, not ours to auto-recover.
     *   2. A walk on a freshly-mounted UFS image cold-reads the
     *      first directory block; we want to keep mount fast. */
    int layout_valid = 0;
    {
        char check[600];
        int cn = snprintf(check, sizeof(check), "%s/sce_sys/param.json",
                          mount_point);
        if (cn > 0 && (size_t)cn < sizeof(check)) {
            struct stat sbuf;
            if (stat(check, &sbuf) == 0 && S_ISREG(sbuf.st_mode)) {
                layout_valid = 1;
            }
        }
    }

    /* Mount-geometry diagnostics. Statfs the resolved mount point so
     * we can surface the actual reported block size, I/O block size,
     * and effective-RO flag back to the client. Lets a user reporting
     * "mount succeeded but games are invisible" share the geometry
     * without ssh — sector-size mismatches between the .ffpkg image
     * and the LVD/MD device are the leading suspect for a UFS image
     * that mounts but reads as empty. Best-effort: a failing statfs
     * leaves the diagnostics fields zeroed and the rest of the ACK
     * intact. */
    uint64_t diag_bsize = 0;
    uint64_t diag_iosize = 0;
    int diag_kernel_ro = 0;
    {
        struct statfs sbuf;
        if (statfs(mount_point, &sbuf) == 0) {
            diag_bsize = (uint64_t)sbuf.f_bsize;
            diag_iosize = (uint64_t)sbuf.f_iosize;
            diag_kernel_ro = (sbuf.f_flags & MNT_RDONLY) ? 1 : 0;
        }
    }

    /* User-facing PS5 toast. The desktop client surfaces the same
     * info inline in the Library row, but firing a toast here gives
     * users still on the PS5 (e.g. running ps5upload-engine in
     * headless mode) a visible confirmation. Truncates to fit the
     * 128-byte stack buffer for the snprintf — pop_notification
     * itself caps at ~3 KiB. The layout-warning suffix on the toast
     * gives the on-couch user the same hint the desktop UI surfaces. */
    {
        char toast[200];
        if (layout_valid) {
            snprintf(toast, sizeof(toast), "Mounted %s at %s%s",
                     fstype, mount_point, read_only ? " (read-only)" : "");
        } else {
            snprintf(toast, sizeof(toast),
                     "Mounted %s at %s%s — but no sce_sys/param.json at "
                     "image root, Register/Launch will fail",
                     fstype, mount_point, read_only ? " (read-only)" : "");
        }
        pop_notification(toast);
    }

    /* JSON-escape every user-controllable string. is_path_allowed
     * accepts paths containing '"' or '\' so a path like
     * /data/foo"bar would unescape into invalid JSON in the ACK.
     * Pre-2.2.52 these sites embedded the raw chars. */
    {
        char mp_esc[768], dev_esc[64], fs_esc[32], img_esc[1024];
        json_escape_into(mount_point, mp_esc,  sizeof(mp_esc));
        json_escape_into(devname,     dev_esc, sizeof(dev_esc));
        json_escape_into(fstype,      fs_esc,  sizeof(fs_esc));
        json_escape_into(image_path,  img_esc, sizeof(img_esc));
        /* Emit bool fields as JSON booleans (true/false), not as
         * integer 0/1. Same root cause as the reuse branch above and
         * round 1's PKG_INSTALL_ACK fix — engine's MountResult has
         * read_only/layout_valid/kernel_ro declared as `bool` and
         * serde rejects integer-for-bool with `invalid type: integer
         * N, expected a boolean`. That's the source of every
         * "decode FS_MOUNT_ACK body as JSON" warning in engine.log. */
        const char *ro_str = read_only ? "true" : "false";
        const char *layout_str = layout_valid ? "true" : "false";
        const char *kernel_ro_str = diag_kernel_ro ? "true" : "false";
        n = snprintf(resp, sizeof(resp),
                     "{\"mount_point\":\"%s\",\"dev_node\":\"%s\","
                     "\"fstype\":\"%s\","
                     "\"source_image\":\"%s\",\"read_only\":%s,"
                     "\"layout_valid\":%s,"
                     "\"f_bsize\":%llu,\"f_iosize\":%llu,\"kernel_ro\":%s}",
                     mp_esc, dev_esc, fs_esc, img_esc,
                     ro_str, layout_str,
                     (unsigned long long)diag_bsize,
                     (unsigned long long)diag_iosize,
                     kernel_ro_str);
    }
    if (n < 0 || (size_t)n >= sizeof(resp)) n = 0;
    return send_frame(client_fd, FTX2_FRAME_FS_MOUNT_ACK, 0, trace_id,
                      resp, (uint64_t)n);
}

/* ── FS_UNMOUNT handler ─────────────────────────────────────────────────
 *
 * Only lets callers unmount things under /mnt/ps5upload/ so a malicious
 * or mistaken request can't tear down system mounts or mounts owned
 * by other tools. Resolves which backend (LVD or MD) backs the mount
 * via getmntinfo
 * and dispatches to the matching detach helper — skipping that would
 * leak the attachment per mount/unmount cycle. */
static int handle_fs_unmount(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *request_body,
                              uint64_t body_len) {
    char mount_point[256] = {0};
    (void)body_len;
    if (!state) return -1;
    if (request_body) {
        extract_json_string_field(request_body, "mount_point",
                                  mount_point, sizeof(mount_point));
    }
    /* Two ways to confirm a mount is ours, ordered cheap-first:
     *   1. /mnt/ps5upload/<leaf> prefix — the legacy namespace,
     *      always exclusively ours.
     *   2. A tracker file exists for this exact mount_point — proves
     *      handle_fs_mount registered this mount on this PS5.
     * Either is sufficient. We deliberately don't accept "lives on
     * an is_path_allowed root" alone, because that would let a user
     * unmount a real /mnt/ext1 they never asked us to manage. The
     * tracker is our consent record. */
    const size_t base_len = strlen(FS_MOUNT_BASE);
    const int legacy_match =
        strncmp(mount_point, FS_MOUNT_BASE "/", base_len + 1) == 0 &&
        mount_point[base_len + 1] != '\0';
    /* Component-aware ".." check matches is_path_allowed's semantics —
     * the substring form rejected legitimate filenames like
     * `My..Game` that the FS_MOUNT side accepts, leaving the user
     * with a successfully-mounted image they couldn't unmount. */
    if (path_has_dotdot_component(mount_point) ||
        (!legacy_match && !mount_tracker_exists(mount_point))) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_unmount_not_our_mount", 24);
    }

    /* Backend detection: match the mount's f_mntfromname against
     * /dev/md<N> or /dev/lvd<N> so we dispatch to the right detach
     * helper after the unmount. Either backend may back a mount
     * depending on what FS_MOUNT's attach pipeline ended up using. */
    int detach_md_unit  = -1;
    int detach_lvd_unit = -1;
    struct statfs *mnts = NULL;
    int nmnts = mntinfo_snapshot(&mnts);
    for (int i = 0; i < nmnts && mnts != NULL; i++) {
        if (strcmp(mnts[i].f_mntonname, mount_point) != 0) continue;
        const char *from = mnts[i].f_mntfromname;
        if (strncmp(from, "/dev/lvd", 8) == 0 &&
            from[8] >= '0' && from[8] <= '9') {
            detach_lvd_unit = atoi(from + 8);
        } else if (strncmp(from, "/dev/md", 7) == 0 &&
                   from[7] >= '0' && from[7] <= '9') {
            detach_md_unit = atoi(from + 7);
        }
        break;
    }
    /* Only the unit numbers (ints) are kept past here; the snapshot is done. */
    free(mnts);

    if (fs_mount_try_unmount(mount_point) != 0) {
        /* 2.2.59: differentiate EBUSY ("game is running, files
         * inside the mount are open") from generic failure. The
         * frontend uses the specific reason to show a
         * "exit the game on the PS5 first" hint instead of the
         * generic "unmount failed" — much more actionable. */
        int saved_errno = errno;
        const char *reason = "fs_unmount_failed";
        size_t reason_len = 17;
        if (saved_errno == EBUSY) {
            reason = "fs_unmount_busy";
            reason_len = 15;
        } else if (saved_errno == EACCES || saved_errno == EPERM) {
            reason = "fs_unmount_permission";
            reason_len = 21;
        }
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, reason_len);
    }
    /* Best-effort detach. Worst case we leave the attachment; a fresh
     * mount of the same image gets a new unit. Never fail the user
     * request on detach alone. */
    if (detach_lvd_unit >= 0) (void)fs_mount_detach_lvd(detach_lvd_unit);
    if (detach_md_unit  >= 0) (void)fs_mount_detach_md(detach_md_unit);
    /* Clean up the (now-empty) mount-point directory. We rmdir only
     * the leaf — for user-chosen deep paths (/mnt/ext1/games/foo) we
     * deliberately leave parent dirs alone since they may have been
     * pre-existing or hold other content. */
    rmdir(mount_point);
    /* Remove the per-mount source tracker. mount_tracker_remove
     * accepts the full mount_point and computes the right tracker
     * key for both legacy /mnt/ps5upload/<name> and user-chosen
     * paths. */
    mount_tracker_remove(mount_point);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Unmounted %s", mount_point);
        pop_notification(toast);
    }

    return send_frame(client_fd, FTX2_FRAME_FS_UNMOUNT_ACK, 0, trace_id,
                      NULL, 0);
}

/* ── APP_* handlers (thin wrappers over register.c) ─────────────────────
 *
 * The heavy lifting lives in payload/src/register.c. These handlers
 * only parse the request body, call into the register module, and
 * turn the returned err_reason
 * string into an ERROR frame. Each handler bumps command_count so
 * STATUS_ACK reflects management-port activity. */

static int handle_app_register(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *request_body,
                                uint64_t body_len) {
    char src_path[512] = {0};
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    char title_name[REGISTER_MAX_TITLE_NAME] = {0};
    char title_name_esc[REGISTER_MAX_TITLE_NAME * 2 + 2];
    char resp[REGISTER_MAX_TITLE_ID + REGISTER_MAX_TITLE_NAME * 2 + 128];
    int used_nullfs = 0;
    const char *err = NULL;
    (void)body_len;
    if (!state) return -1;
    /* APP_REGISTER goes through register.c::register_title_from_path,
     * which mutexes every Sony install/launch/uninstall call via
     * g_register_lock. Compile-time linkage of the Sony sprxes (see
     * Makefile) ensures rtld initialises sprx state via DT_NEEDED
     * before main runs — earlier dlopen-at-runtime paths wedged on
     * FW 9.60 because the sprx init order was wrong.
     *
     * If a single call still hangs on a future firmware, the mutex
     * keeps the blast radius to this code path; the rest of the
     * payload (FS, HW, transfer, takeover) keeps serving. */
    int patch_drm_type = 0;
    if (request_body) {
        extract_json_string_field(request_body, "src_path",
                                  src_path, sizeof(src_path));
        /* Numeric 1 means "yes, patch param.json's
         * applicationDrmType to standard before staging". 0 or
         * absent leaves the source untouched. Same JSON-uint
         * helper as fs_mount's read_only flag. */
        patch_drm_type =
            (extract_json_uint64_field(request_body, "patch_drm_type") != 0) ? 1 : 0;
    }
    if (src_path[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_src_path_missing", 25);
    }
    if (!is_path_allowed(src_path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_src_path_not_allowed", 29);
    }
    if (register_title_from_path(src_path, patch_drm_type, title_id,
                                  title_name, &used_nullfs, &err) != 0) {
        const char *reason = err ? err : "register_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    json_escape_into(title_name, title_name_esc, sizeof(title_name_esc));
    /* title_id was vetted by register.c::is_safe_component which
     * blocks `.`, `..`, and `/` — but NOT `"` or `\`. A malicious
     * homebrew param.json with a crafted titleId could otherwise
     * produce a malformed JSON response (engine's serde_json would
     * reject and surface a confusing parse error to the user
     * instead of the actual register-success). Escape defensively. */
    char title_id_esc[REGISTER_MAX_TITLE_ID * 2 + 2];
    json_escape_into(title_id, title_id_esc, sizeof(title_id_esc));
    int n = snprintf(resp, sizeof(resp),
                     "{\"title_id\":\"%s\",\"title_name\":\"%s\","
                     "\"used_nullfs\":%s}",
                     title_id_esc, title_name_esc,
                     used_nullfs ? "true" : "false");
    if (n < 0 || (size_t)n >= sizeof(resp)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_response_overflow", 26);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Registered %s (%s)",
                 title_name[0] ? title_name : title_id, title_id);
        pop_notification(toast);
    }

    return send_frame(client_fd, FTX2_FRAME_APP_REGISTER_ACK, 0, trace_id,
                      resp, (uint64_t)n);
}

static int handle_app_unregister(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *request_body,
                                  uint64_t body_len) {
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    const char *err = NULL;
    (void)body_len;
    if (!state) return -1;
    /* Same gate-lifted rationale as APP_REGISTER above — relies on the
     * compile-time Sony-sprx linkage in the Makefile for proper sprx
     * init before main(). The underlying register.c::unregister_title
     * runs under g_register_lock and degrades cleanly when Sony's
     * sceAppInstUtilAppUnInstall is missing on a firmware (the nullfs
     * teardown alone is enough to remove the XMB tile). */
    if (request_body) {
        extract_json_string_field(request_body, "title_id",
                                  title_id, sizeof(title_id));
    }
    if (title_id[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "unregister_title_id_missing", 27);
    }
    if (unregister_title(title_id, &err) != 0) {
        const char *reason = err ? err : "unregister_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Unregistered %s", title_id);
        pop_notification(toast);
    }

    return send_frame(client_fd, FTX2_FRAME_APP_UNREGISTER_ACK, 0,
                      trace_id, NULL, 0);
}

static int handle_app_launch(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *request_body,
                              uint64_t body_len) {
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    const char *err = NULL;
    (void)body_len;
    if (!state) return -1;
    /* APP_LAUNCH calls register.c::launch_title which runs the
     * triple-strategy chain (sceLncUtilLaunchApp with populated
     * 24-byte LncAppParam, then NULL param, then
     * sceSystemServiceLaunchApp) under g_register_lock. Relies on
     * compile-time Sony-sprx linkage in the Makefile for proper
     * sprx init before main. The primary path actually used on
     * FW 9.60 is the ShellUI ptrace RPC inside launch_title — this
     * direct-call chain is the fallback for firmwares where the
     * caller-pid check is looser. */
    if (request_body) {
        extract_json_string_field(request_body, "title_id",
                                  title_id, sizeof(title_id));
    }
    if (title_id[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "launch_title_id_missing", 23);
    }
    /* Stack scratch for launch_title's formatted failure reasons —
     * per-call storage so concurrent APP_LAUNCH mgmt threads can't
     * race each other's error strings (see register.h). */
    char launch_reason[128];
    if (launch_title(title_id, &err, launch_reason, sizeof(launch_reason)) !=
        0) {
        const char *reason = err ? err : "launch_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Launching %s", title_id);
        pop_notification(toast);
    }

    return send_frame(client_fd, FTX2_FRAME_APP_LAUNCH_ACK, 0,
                      trace_id, NULL, 0);
}

static int handle_app_list_registered(runtime_state_t *state, int client_fd,
                                       uint64_t trace_id,
                                       const char *request_body,
                                       uint64_t body_len) {
    /* Buffer sized for a max-library PS5 with generous headroom. Each
     * entry is ~135 bytes after we dropped the title_name echo
     * ({"title_id":"PPSA00xxx","title_name":"PPSA00xxx","src":"/mnt/.../game","image_backed":false},
     * conservative estimate). 512 KiB holds ~3800 entries -- well past
     * Sony's own UI limit (~1.5k titles in practice) with room to grow
     * if a user has a fragmented app.db full of stale registrations. */
    const size_t cap = 512u * 1024u;
    char *buf = NULL;
    size_t written = 0;
    const char *err = NULL;
    int rc;
    (void)request_body;
    (void)body_len;
    if (!state) return -1;
    buf = (char *)malloc(cap);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "list_registered_oom", 19);
    }
    if (list_registered_titles_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "list_registered_failed";
        rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                        reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_APP_LIST_REGISTERED_ACK, 0,
                    trace_id, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Hardware monitoring handlers (thin wrappers over hw_info.c) ──────── */

static int handle_hw_text_op(runtime_state_t *state, int client_fd,
                              uint64_t trace_id,
                              int (*getter)(char *, size_t, size_t *, const char **),
                              uint16_t ack_type, const char *default_err) {
    char body[2048];
    size_t written = 0;
    const char *err = NULL;
    if (!state) return -1;
    if (getter(body, sizeof(body), &written, &err) != 0) {
        const char *reason = err ? err : default_err;
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, ack_type, 0, trace_id, body, (uint64_t)written);
}

static int handle_hw_info(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_info_get_text,
                              FTX2_FRAME_HW_INFO_ACK, "hw_info_failed");
}

/* HW_TEMPS. A request body of "1" selects the EXTENDED read (SoC power /
 * CPU usage / fan duty / product shape) used by the explicit "Read
 * sensors" click; an empty body (the Dashboard's 5 s auto-poll) gets the
 * BASIC, always-safe read. Gating the risky getters on the body keeps
 * them off every auto-poll path — see hw_temps_get_text_ex. */
static int handle_hw_temps(runtime_state_t *state, int client_fd, uint64_t trace_id,
                           const char *request_body, uint64_t body_len) {
    /* Body selects the EXTENDED telemetry: "1" = all (back-compat with the
     * engine), or any subset of the chars p/u/f/s to read just those
     * getters — lets the desktop (or a probe) exclude a call that wedges a
     * given firmware. Empty body = basic. */
    int flags = 0;
    if (request_body && body_len >= 1) {
        if (request_body[0] == '1') {
            flags = HW_EXT_ALL;
        } else {
            for (uint64_t i = 0; i < body_len; i++) {
                switch (request_body[i]) {
                    case 'p': flags |= HW_EXT_POWER; break;
                    case 'u': flags |= HW_EXT_USAGE; break;
                    case 'f': flags |= HW_EXT_FAN;   break;
                    case 's': flags |= HW_EXT_SHAPE; break;
                    default: break;
                }
            }
        }
    }
    char body[2048];
    size_t written = 0;
    const char *err = NULL;
    if (!state) return -1;
    if (hw_temps_get_text_ex(flags, body, sizeof(body), &written, &err) != 0) {
        const char *reason = err ? err : "hw_temps_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_HW_TEMPS_ACK, 0, trace_id,
                      body, (uint64_t)written);
}

static int handle_hw_power(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_power_get_text,
                              FTX2_FRAME_HW_POWER_ACK, "hw_power_failed");
}

/* ── System clock get/set (sys_time.c wrappers) ────────────────────────── */

/* Tiny ASCII-digit JSON field reader. Pulls "<name>":N out of a JSON
 * blob; returns 1 on success + writes *out, 0 otherwise. Doesn't
 * handle quoted-string values (we don't need them for the time-set
 * request, which is integer-only). Tolerates whitespace between
 * `:` and the digits. Used in handle_time_set below. */
static int json_read_int_field(const char *body, size_t body_len,
                                const char *name, long *out) {
    char needle[32];
    int needle_len = snprintf(needle, sizeof(needle), "\"%s\"", name);
    if (needle_len <= 0 || (size_t)needle_len >= sizeof(needle)) return 0;
    if (body_len < (size_t)needle_len) return 0;
    const char *body_end = body + body_len;
    const char *p = find_bounded(body, body_len, needle);
    if (!p) return 0;
    p += needle_len;
    while (p < body_end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
    if (p >= body_end || *p != ':') return 0;
    p++;
    while (p < body_end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
    if (p >= body_end) return 0;
    int neg = 0;
    if (*p == '-') { neg = 1; p++; }
    if (p >= body_end || *p < '0' || *p > '9') return 0;
    long v = 0;
    while (p < body_end && *p >= '0' && *p <= '9') {
        /* Guard against overflow of the fields the caller cares about
         * (year/month/day/...). 10-digit cap is enough for any sane
         * input; out-of-range values fail the per-field validation
         * inside sys_time_set anyway. */
        if (v > 100000000L) return 0;
        v = v * 10 + (*p - '0');
        p++;
    }
    *out = neg ? -v : v;
    return 1;
}

static int handle_time_get(runtime_state_t *state, int client_fd,
                            uint64_t trace_id) {
    if (!state) return -1;
    sce_datetime_t dt;
    memset(&dt, 0, sizeof(dt));
    uint32_t ec = 0;
    int rc = sys_time_get(&dt, &ec);
    char body[256];
    int n;
    if (rc == 0) {
        n = snprintf(body, sizeof(body),
                     "{\"ok\":true,\"err_code\":0,"
                     "\"year\":%u,\"month\":%u,\"day\":%u,"
                     "\"hour\":%u,\"min\":%u,\"sec\":%u}",
                     (unsigned)dt.year, (unsigned)dt.month, (unsigned)dt.day,
                     (unsigned)dt.hour, (unsigned)dt.minute, (unsigned)dt.second);
    } else {
        n = snprintf(body, sizeof(body),
                     "{\"ok\":false,\"err_code\":%u}",
                     (unsigned)ec);
    }
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err_code\":0}";
        return send_frame(client_fd, FTX2_FRAME_TIME_GET_ACK, 0, trace_id,
                          fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_TIME_GET_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

static int handle_time_set(runtime_state_t *state, int client_fd,
                            uint64_t trace_id,
                            const char *request_body, uint64_t body_len) {
    if (!state) return -1;
    if (!request_body || body_len == 0) {
        const char *err = "{\"ok\":false,\"err_code\":3758104577}"; /* SYS_TIME_ERR_NULL_ARG */
        return send_frame(client_fd, FTX2_FRAME_TIME_SET_ACK, 0, trace_id,
                          err, (uint64_t)strlen(err));
    }
    /* Pull each field. Missing fields default to zero, which the
     * sys_time_set range check will reject — caller mistake produces
     * a clean rc=-1 with SYS_TIME_ERR_NULL_ARG-style err_code. */
    long year = 0, month = 0, day = 0, hour = 0, minute = 0, second = 0;
    int ok_year = json_read_int_field(request_body, (size_t)body_len, "year",  &year);
    int ok_mon  = json_read_int_field(request_body, (size_t)body_len, "month", &month);
    int ok_day  = json_read_int_field(request_body, (size_t)body_len, "day",   &day);
    int ok_hr   = json_read_int_field(request_body, (size_t)body_len, "hour",  &hour);
    int ok_min  = json_read_int_field(request_body, (size_t)body_len, "min",   &minute);
    int ok_sec  = json_read_int_field(request_body, (size_t)body_len, "sec",   &second);
    if (!ok_year || !ok_mon || !ok_day || !ok_hr || !ok_min || !ok_sec ||
        year < 1970 || year > 2200 || month < 1 || month > 12 ||
        day < 1 || day > 31 || hour < 0 || hour > 23 ||
        minute < 0 || minute > 59 || second < 0 || second > 59) {
        const char *err = "{\"ok\":false,\"err_code\":3758104577}"; /* SYS_TIME_ERR_NULL_ARG */
        return send_frame(client_fd, FTX2_FRAME_TIME_SET_ACK, 0, trace_id,
                          err, (uint64_t)strlen(err));
    }
    sce_datetime_t dt;
    memset(&dt, 0, sizeof(dt));
    dt.year   = (uint16_t)year;
    dt.month  = (uint16_t)month;
    dt.day    = (uint16_t)day;
    dt.hour   = (uint16_t)hour;
    dt.minute = (uint16_t)minute;
    dt.second = (uint16_t)second;
    uint32_t ec = 0;
    int64_t prior_unix = -1, new_unix = -1;
    int rc = sys_time_set(&dt, &ec, &prior_unix, &new_unix);
    char body[256];
    int n = snprintf(body, sizeof(body),
                     "{\"ok\":%s,\"err_code\":%u,"
                     "\"prior_unix\":%lld,\"new_unix\":%lld}",
                     rc == 0 ? "true" : "false",
                     (unsigned)ec,
                     (long long)prior_unix,
                     (long long)new_unix);
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err_code\":0}";
        return send_frame(client_fd, FTX2_FRAME_TIME_SET_ACK, 0, trace_id,
                          fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_TIME_SET_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

/* ── PS5 Date & Time state (registry-backed) ─────────────────────────────
 *
 * Reads (TIME_STATE_GET) and writes (TIME_STATE_SET) the SCE registry
 * DATE_* keyspace — timezone, DST policy, date/time format,
 * auto-sync (NTP) flag, tzdata version, NTP-error counter — plus
 * the libSceRtc NTP-derived tick (cached, no fresh sync).
 *
 * GET is straightforward: one read per key, JSON-encode with
 * per-field `*_avail` flags so the desktop can grey out fields the
 * payload couldn't read on this firmware (Sony's runtime exports
 * vary per FW; not all DATE_* keys may be reachable everywhere).
 *
 * SET takes a JSON request with OPTIONAL fields — only present
 * fields get written. The response surfaces per-field rc + err_code
 * so the user can see "set_auto succeeded but tz_index was
 * rejected" instead of one opaque ok/fail. Same ucred-elevation
 * envelope as TIME_SET.
 *
 * Novel territory in 2.10.0 — first public PS5 homebrew to write
 * to this namespace. See reference_ps5_date_registry_keys.md for
 * the hardware-verification status of each key. */

/* Helper: write a `"<name>":<int>,"<name>_avail":<bool>` JSON pair
 * for one registry int field, given the read rc + value. Returns
 * bytes written. Caller appends the trailing comma if more fields
 * follow. Used to keep handle_time_state_get's snprintf chain
 * readable instead of 10 separate conditional branches. */
static int append_state_int_field(char *out, size_t cap,
                                   const char *name,
                                   int rc, int val, uint32_t err) {
    if (rc == 0) {
        return snprintf(out, cap,
                        "\"%s\":%d,\"%s_avail\":true,\"%s_err\":0",
                        name, val, name, name);
    }
    return snprintf(out, cap,
                    "\"%s\":0,\"%s_avail\":false,\"%s_err\":%u",
                    name, name, name, (unsigned)err);
}

static int handle_time_state_get(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id) {
    if (!state) return -1;

    /* Read every key. None of these failing should abort the
     * response — the desktop wants partial data with per-field
     * availability so it can render a "tz_index unreadable on this
     * firmware" tooltip rather than an empty card. */
    int tz_index = 0;          uint32_t tz_err = 0;
    int date_fmt = 0;          uint32_t date_fmt_err = 0;
    int time_fmt = 0;          uint32_t time_fmt_err = 0;
    int summer_pol = 0;        uint32_t summer_pol_err = 0;
    int set_auto = 0;          uint32_t set_auto_err = 0;
    int is_summer = 0;         uint32_t is_summer_err = 0;
    int utc_off_sec = 0;       uint32_t utc_off_sec_err = 0;
    int tz_off_min = 0;        uint32_t tz_off_min_err = 0;
    int rtc_err_count = 0;     uint32_t rtc_err_count_err = 0;
    char tzdata_ver[32] = {0}; uint32_t tzdata_ver_err = 0;
    int64_t ntp_tick_unix = -1; uint32_t ntp_tick_err = 0;
    sce_datetime_t wall_dt;     uint32_t wall_err = 0;
    memset(&wall_dt, 0, sizeof(wall_dt));

    int tz_rc          = sys_registry_get_int(SCE_KEY_DATE_TIME_ZONE,
                                                &tz_index, &tz_err);
    int date_fmt_rc    = sys_registry_get_int(SCE_KEY_DATE_DATE_FORMAT,
                                                &date_fmt, &date_fmt_err);
    int time_fmt_rc    = sys_registry_get_int(SCE_KEY_DATE_TIME_FORMAT,
                                                &time_fmt, &time_fmt_err);
    int summer_pol_rc  = sys_registry_get_int(SCE_KEY_DATE_SUMMER_TIME,
                                                &summer_pol, &summer_pol_err);
    int set_auto_rc    = sys_registry_get_int(SCE_KEY_DATE_SET_AUTO,
                                                &set_auto, &set_auto_err);
    int is_summer_rc   = sys_registry_get_int(SCE_KEY_DATE_IS_SUMMER_TIME,
                                                &is_summer, &is_summer_err);
    int utc_off_rc     = sys_registry_get_int(SCE_KEY_DATE_UTC_OFFSET,
                                                &utc_off_sec, &utc_off_sec_err);
    int tz_off_rc      = sys_registry_get_int(SCE_KEY_DATE_TIMEZONE_OFFSET,
                                                &tz_off_min, &tz_off_min_err);
    int rtc_err_rc     = sys_registry_get_int(SCE_KEY_DATE_RTC_ERROR_COUNT,
                                                &rtc_err_count, &rtc_err_count_err);
    int tzdata_rc      = sys_registry_get_str(SCE_KEY_DATE_TZDATA_UPDATE,
                                                tzdata_ver, sizeof(tzdata_ver),
                                                &tzdata_ver_err);
    int ntp_tick_rc    = sys_registry_get_ntp_tick_unix(&ntp_tick_unix,
                                                          &ntp_tick_err);
    int wall_rc        = sys_time_get(&wall_dt, &wall_err);

    /* Build response. JSON grows up to ~1.2 KB with all fields
     * populated; sizing to 2 KB gives plenty of slack for the
     * per-field err_code expansions. Each append_state_int_field
     * returns the bytes written; we accumulate `off` and check for
     * truncation after every append (snprintf semantics: returns
     * the bytes that WOULD have been written, possibly > cap-left). */
    char body[2048];
    char *p = body;
    size_t cap = sizeof(body);
    int n;

    n = snprintf(p, cap, "{\"ok\":true,");
    if (n < 0 || (size_t)n >= cap) goto truncated;
    p += n; cap -= (size_t)n;

#define APPEND_INT_FIELD(name, rc, val, err) do { \
    n = append_state_int_field(p, cap, name, rc, val, err); \
    if (n < 0 || (size_t)n >= cap) goto truncated; \
    p += n; cap -= (size_t)n; \
    if (cap < 2) goto truncated; \
    *p++ = ','; cap -= 1; \
} while (0)

    APPEND_INT_FIELD("tz_index",         tz_rc,          tz_index,      tz_err);
    APPEND_INT_FIELD("date_format",      date_fmt_rc,    date_fmt,      date_fmt_err);
    APPEND_INT_FIELD("time_format",      time_fmt_rc,    time_fmt,      time_fmt_err);
    APPEND_INT_FIELD("summer_policy",    summer_pol_rc,  summer_pol,    summer_pol_err);
    APPEND_INT_FIELD("set_auto",         set_auto_rc,    set_auto,      set_auto_err);
    APPEND_INT_FIELD("is_summer_time",   is_summer_rc,   is_summer,     is_summer_err);
    APPEND_INT_FIELD("utc_offset_sec",   utc_off_rc,     utc_off_sec,   utc_off_sec_err);
    APPEND_INT_FIELD("tz_offset_min",    tz_off_rc,      tz_off_min,    tz_off_min_err);
    APPEND_INT_FIELD("rtc_error_count",  rtc_err_rc,     rtc_err_count, rtc_err_count_err);

#undef APPEND_INT_FIELD

    /* tzdata version (string). JSON-escape isn't strictly needed
     * since Sony's format is `[0-9a-z.]+` (e.g. "2023d"), but be
     * defensive — pass through any printable ASCII and refuse the
     * non-printables. */
    char tzdata_safe[64];
    {
        size_t si = 0;
        for (size_t i = 0; i < sizeof(tzdata_ver) && tzdata_ver[i] != '\0' &&
             si + 1 < sizeof(tzdata_safe); i++) {
            unsigned char c = (unsigned char)tzdata_ver[i];
            if (c >= 0x20 && c <= 0x7E && c != '"' && c != '\\') {
                tzdata_safe[si++] = (char)c;
            } else {
                tzdata_safe[si++] = '?';
            }
        }
        tzdata_safe[si] = '\0';
    }
    n = snprintf(p, cap,
                  "\"tzdata\":\"%s\",\"tzdata_avail\":%s,\"tzdata_err\":%u,",
                  tzdata_safe,
                  tzdata_rc == 0 ? "true" : "false",
                  (unsigned)tzdata_ver_err);
    if (n < 0 || (size_t)n >= cap) goto truncated;
    p += n; cap -= (size_t)n;

    /* NTP tick (cached, signed unix seconds). -1 sentinel when read
     * failed; the desktop computes drift only when both ntp_tick and
     * wall_clock_unix are non-negative. */
    n = snprintf(p, cap,
                  "\"ntp_tick_unix\":%lld,\"ntp_tick_avail\":%s,\"ntp_tick_err\":%u,",
                  (long long)ntp_tick_unix,
                  ntp_tick_rc == 0 ? "true" : "false",
                  (unsigned)ntp_tick_err);
    if (n < 0 || (size_t)n >= cap) goto truncated;
    p += n; cap -= (size_t)n;

    /* Wall clock as the same epoch shape, derived from the
     * sce_datetime_t we already read. Computed via the same UTC-only
     * convention sys_time_set uses for prior/new_unix in TIME_SET_ACK
     * — keeps drift comparisons apples-to-apples. */
    int64_t wall_unix = -1;
    if (wall_rc == 0) {
        struct tm tm;
        memset(&tm, 0, sizeof(tm));
        tm.tm_year = (int)wall_dt.year - 1900;
        tm.tm_mon  = (int)wall_dt.month - 1;
        tm.tm_mday = (int)wall_dt.day;
        tm.tm_hour = (int)wall_dt.hour;
        tm.tm_min  = (int)wall_dt.minute;
        tm.tm_sec  = (int)wall_dt.second;
        time_t t = timegm(&tm);
        if (t != (time_t)-1) wall_unix = (int64_t)t;
    }
    n = snprintf(p, cap,
                  "\"wall_clock_unix\":%lld,\"wall_clock_avail\":%s,\"wall_clock_err\":%u}",
                  (long long)wall_unix,
                  wall_rc == 0 ? "true" : "false",
                  (unsigned)wall_err);
    if (n < 0 || (size_t)n >= cap) goto truncated;
    p += n;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_TIME_STATE_GET_ACK, 0, trace_id,
                      body, (uint64_t)(p - body));

truncated: {
    /* Last-resort fallback — any field above blew the buffer.
     * Shouldn't happen at 2 KB but the alternative (return -1 and
     * drop the connection) is worse for the user than a stub
     * response. */
    const char *fb = "{\"ok\":false,\"err_code\":0,\"truncated\":true}";
    return send_frame(client_fd, FTX2_FRAME_TIME_STATE_GET_ACK, 0, trace_id,
                      fb, (uint64_t)strlen(fb));
}
}

static int handle_time_state_set(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id,
                                   const char *request_body,
                                   uint64_t body_len) {
    if (!state) return -1;
    if (!request_body || body_len == 0) {
        const char *err = "{\"ok\":false,\"err_code\":3758108673}"; /* SYS_REGISTRY_ERR_NULL_ARG */
        return send_frame(client_fd, FTX2_FRAME_TIME_STATE_SET_ACK, 0, trace_id,
                          err, (uint64_t)strlen(err));
    }

    /* Optional fields. json_read_int_field returns 0 if the key
     * isn't present — we use that as "skip this write." This is
     * partial-update semantics: caller sends {"set_auto":1} and we
     * only touch set_auto, leaving tz_index etc. as-is. */
    long tz_idx = 0, date_fmt = 0, time_fmt = 0, summer = 0, set_auto = 0;
    int has_tz       = json_read_int_field(request_body, (size_t)body_len, "tz_index",      &tz_idx);
    int has_date_fmt = json_read_int_field(request_body, (size_t)body_len, "date_format",   &date_fmt);
    int has_time_fmt = json_read_int_field(request_body, (size_t)body_len, "time_format",   &time_fmt);
    int has_summer   = json_read_int_field(request_body, (size_t)body_len, "summer_policy", &summer);
    int has_set_auto = json_read_int_field(request_body, (size_t)body_len, "set_auto",      &set_auto);

    /* Range-clamp the writeable fields to documented Sony values
     * before passing them through. Rejecting out-of-range is safer
     * than letting Sony do something undefined with e.g.
     * date_format=99 — the Settings UI would then have to round-trip
     * through "weird state" to recover. */
    if (has_date_fmt && (date_fmt < 0 || date_fmt > 2))     has_date_fmt = 0;
    if (has_time_fmt && (time_fmt < 0 || time_fmt > 1))     has_time_fmt = 0;
    if (has_summer   && (summer   < 0 || summer   > 2))     has_summer   = 0;
    if (has_set_auto && (set_auto < 0 || set_auto > 1))     has_set_auto = 0;
    /* tz_index is an enum into Sony's tzdata table (~120 entries);
     * we don't have the exact upper bound for every firmware so
     * accept any non-negative int. A wrong value is easily reset
     * via Settings → Date and Time. */
    if (has_tz       && tz_idx    < 0)                        has_tz       = 0;

    /* Issue each write. Each populates its own rc + err_code. */
    int rc_tz = 1, rc_date = 1, rc_time = 1, rc_summer = 1, rc_auto = 1;
    uint32_t ec_tz = 0, ec_date = 0, ec_time = 0, ec_summer = 0, ec_auto = 0;
    if (has_tz)       rc_tz     = sys_registry_set_int(SCE_KEY_DATE_TIME_ZONE,    (int)tz_idx,     &ec_tz);
    if (has_date_fmt) rc_date   = sys_registry_set_int(SCE_KEY_DATE_DATE_FORMAT,  (int)date_fmt,   &ec_date);
    if (has_time_fmt) rc_time   = sys_registry_set_int(SCE_KEY_DATE_TIME_FORMAT,  (int)time_fmt,   &ec_time);
    if (has_summer)   rc_summer = sys_registry_set_int(SCE_KEY_DATE_SUMMER_TIME,  (int)summer,     &ec_summer);
    if (has_set_auto) rc_auto   = sys_registry_set_int(SCE_KEY_DATE_SET_AUTO,     (int)set_auto,   &ec_auto);

    /* `ok` is true only if EVERY attempted write succeeded. Skipped
     * writes don't count against ok — they leave rc_* = 1 (untouched)
     * which we filter below. */
    int any_attempted = has_tz || has_date_fmt || has_time_fmt || has_summer || has_set_auto;
    int all_ok = 1;
    if (has_tz       && rc_tz     != 0) all_ok = 0;
    if (has_date_fmt && rc_date   != 0) all_ok = 0;
    if (has_time_fmt && rc_time   != 0) all_ok = 0;
    if (has_summer   && rc_summer != 0) all_ok = 0;
    if (has_set_auto && rc_auto   != 0) all_ok = 0;

    char body[768];
    int n = snprintf(body, sizeof(body),
                      "{\"ok\":%s,\"any_attempted\":%s,"
                      "\"tz_index_attempted\":%s,\"tz_index_rc\":%d,\"tz_index_err\":%u,"
                      "\"date_format_attempted\":%s,\"date_format_rc\":%d,\"date_format_err\":%u,"
                      "\"time_format_attempted\":%s,\"time_format_rc\":%d,\"time_format_err\":%u,"
                      "\"summer_policy_attempted\":%s,\"summer_policy_rc\":%d,\"summer_policy_err\":%u,"
                      "\"set_auto_attempted\":%s,\"set_auto_rc\":%d,\"set_auto_err\":%u}",
                      (all_ok && any_attempted) ? "true" : "false",
                      any_attempted ? "true" : "false",
                      has_tz ? "true" : "false",       has_tz ? rc_tz : 0,         (unsigned)ec_tz,
                      has_date_fmt ? "true" : "false", has_date_fmt ? rc_date : 0, (unsigned)ec_date,
                      has_time_fmt ? "true" : "false", has_time_fmt ? rc_time : 0, (unsigned)ec_time,
                      has_summer ? "true" : "false",   has_summer ? rc_summer : 0, (unsigned)ec_summer,
                      has_set_auto ? "true" : "false", has_set_auto ? rc_auto : 0, (unsigned)ec_auto);
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err_code\":0,\"truncated\":true}";
        return send_frame(client_fd, FTX2_FRAME_TIME_STATE_SET_ACK, 0, trace_id,
                          fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_TIME_STATE_SET_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

/* ── SMP metadata self-healer ───────────────────────────────────────────
 *
 * Thin façade over smp_meta.c primitives. Two frames:
 *   SMP_META_CONTROL — action=start | run_now | set_poll (with interval)
 *   SMP_META_STATS   — read-only stats snapshot
 *
 * `action` parsing uses literal-substring matching rather than a JSON
 * string-field reader because the three keywords are unique and never
 * appear as a substring of each other, so the simpler approach can't
 * misfire. The runtime.c-wide JSON helpers (json_read_int_field) handle
 * the `interval` numeric. Action precedence (start > set_poll > run_now)
 * matters only if the caller sends multiple in one frame — we treat
 * that as a single highest-precedence operation rather than chaining. */

static int handle_smp_meta_control(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id,
                                   const char *request_body,
                                   uint64_t body_len) {
    if (!state) return -1;

    /* Empty body defaults to a no-op stats-only ACK so a probe call
     * doesn't accidentally start the watcher. Desktop should send
     * explicit {"action":"start"} when it actually wants the worker. */
    int do_start    = 0;
    int do_run_now  = 0;
    int do_set_poll = 0;
    long interval   = 0;

    if (request_body && body_len > 0 && body_len < 4096) {
        /* Locate the `"action"` key, then read the next quoted string
         * value. Naive substring scan would mis-fire on bodies like
         * `{"action":"set_poll","note":"\"start\""}` — three actions
         * would all match. Anchoring on the `"action":` key + reading
         * exactly the next quoted token gives us strict JSON-aware
         * dispatch.
         *
         * Implementation: scan for `"action"` followed by optional
         * whitespace + `:` + optional whitespace + opening quote, then
         * read up to the closing quote into a small stack buffer. The
         * payload may not be NUL-terminated, so all reads stay inside
         * body_len. */
        static const char ACTION_KEY[] = "\"action\"";
        const size_t key_len = sizeof(ACTION_KEY) - 1;
        size_t i = 0;
        while (i + key_len <= (size_t)body_len) {
            if (memcmp(request_body + i, ACTION_KEY, key_len) != 0) {
                i++;
                continue;
            }
            size_t j = i + key_len;
            while (j < (size_t)body_len &&
                   (request_body[j] == ' ' || request_body[j] == '\t')) j++;
            if (j >= (size_t)body_len || request_body[j] != ':') break;
            j++;
            while (j < (size_t)body_len &&
                   (request_body[j] == ' ' || request_body[j] == '\t')) j++;
            if (j >= (size_t)body_len || request_body[j] != '"') break;
            j++;
            char action_buf[16];
            size_t alen = 0;
            int reject = 0;
            while (j < (size_t)body_len && request_body[j] != '"' &&
                   alen + 1 < sizeof(action_buf)) {
                unsigned char ch = (unsigned char)request_body[j++];
                /* Reject control chars + backslash. An attacker who
                 * sends a raw NUL or \xFF mid-string could otherwise
                 * truncate action_buf inside this loop and have us
                 * strcmp against a short prefix that happens to match
                 * "start" / "run_now" / "set_poll". Backslash is
                 * rejected so unhandled JSON escapes (st…)
                 * can't bypass strict matching either. */
                if (ch < 0x20 || ch == 0x7F || ch == '\\') {
                    reject = 1;
                    break;
                }
                action_buf[alen++] = (char)ch;
            }
            action_buf[alen] = '\0';
            if (!reject) {
                if      (!strcmp(action_buf, "start"))    do_start    = 1;
                else if (!strcmp(action_buf, "run_now"))  do_run_now  = 1;
                else if (!strcmp(action_buf, "set_poll")) do_set_poll = 1;
            }
            break;
        }
        if (do_set_poll) {
            /* Check rc — `json_read_int_field` returns 0 when the key
             * is missing or malformed, leaving `interval` at its 0
             * initializer. Without this check we'd silently call
             * `set_poll_seconds(0)` which the payload clamps to MIN=5
             * — benign today but tomorrow's MIN tightening would
             * surface as "user said 30 but worker sweeps every 5s".
             * Refusing set_poll when interval is missing keeps the
             * UI's slider value the source of truth. */
            if (json_read_int_field(request_body, (size_t)body_len,
                                     "interval", &interval) != 1) {
                do_set_poll = 0;
            }
        }
    }

    int err = 0;
    if (do_start) {
        if (smp_meta_init() != 0) err = 1;
    }
    /* run_now is harmless before init (it just sets a flag the worker
     * will read once started); set_poll likewise just updates the
     * atomic. So we run them whether or not start was issued. */
    if (do_run_now) (void)smp_meta_run_now();
    int poll = smp_meta_get_poll_seconds();
    if (do_set_poll) poll = smp_meta_set_poll_seconds((int)interval);

    char body[160];
    int n;
    if (err) {
        n = snprintf(body, sizeof(body),
                     "{\"ok\":false,\"err\":\"pthread_create_failed\","
                     "\"poll_seconds\":%d}", poll);
    } else {
        n = snprintf(body, sizeof(body),
                     "{\"ok\":true,\"poll_seconds\":%d}", poll);
    }
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err\":\"truncated\"}";
        return send_frame(client_fd, FTX2_FRAME_SMP_META_CONTROL_ACK, 0,
                          trace_id, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_SMP_META_CONTROL_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

static int handle_smp_meta_stats(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id) {
    if (!state) return -1;

    smp_meta_stats_t s;
    smp_meta_get_stats(&s);

    /* JSON-escape last_missing minimally: TITLE_ID only contains
     * [A-Z0-9], so no escaping is needed. We still copy through a
     * sanity bound to prevent any non-printable from leaking if the
     * field gets corrupted upstream. */
    char tid[64];
    size_t j = 0;
    for (size_t i = 0; i < sizeof(s.last_missing) && s.last_missing[i]; i++) {
        unsigned char c = (unsigned char)s.last_missing[i];
        if (c < 0x20 || c > 0x7E || c == '"' || c == '\\') break;
        if (j + 1 >= sizeof(tid)) break;
        tid[j++] = (char)c;
    }
    tid[j] = '\0';

    char body[384];
    int n = snprintf(body, sizeof(body),
        "{\"running\":%s,\"poll_seconds\":%d,\"last_run_unix\":%llu,"
        "\"games_scanned\":%d,\"icons_healed\":%d,\"pics_healed\":%d,"
        "\"json_healed\":%d,\"still_missing\":%d,\"last_missing\":\"%s\"}",
        s.running ? "true" : "false",
        s.poll_seconds,
        (unsigned long long)s.last_run_unix,
        s.games_scanned, s.icons_healed, s.pics_healed,
        s.json_healed, s.still_missing, tid);
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err\":\"truncated\"}";
        return send_frame(client_fd, FTX2_FRAME_SMP_META_STATS_ACK, 0,
                          trace_id, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_SMP_META_STATS_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

/* ── System control (reboot / shutdown / standby / wake-tick) ─────────── */

/* Sony API declarations — these live in libSceSystemService (already in
 * Makefile LIBS). We forward-declare here rather than #include because
 * the SDK doesn't ship a public header for the "request" family. */
extern int sceSystemServiceRequestPowerOff(void);
extern int sceSystemServiceRequestReboot(void);
extern int sceSystemServicePowerTick(void);
/* ICC telemetry — runtime-resolved via dlsym so a missing symbol on
 * a given firmware doesn't break the entire payload at load time.
 *
 * Empirical (2026-05-10): `sceKernelIccGetThermalAlert` is exported
 * by the SDK's libkernel_web.so STUB but NOT by the actual on-PS5
 * libkernel_web.sprx on at least one firmware in the field. With it
 * declared `extern` (compile-time linkage), rtld's lib_init step
 * fails when our binary loads — main() never runs, no port bind, no
 * toast, silent failure. dlsym pattern lets the binary load and just
 * leaves the function pointer NULL when missing; call sites null-check
 * and substitute "err" in the response.
 *
 * Same pattern preemptively applied to the other Icc Get* symbols so
 * a future Sony firmware change (removing more) doesn't repeat the
 * outage. The Control* power-state functions are dlsym'd for the same
 * reason in their own block. */
typedef int (*sce_icc_u32_fn)(unsigned int *out);
typedef int (*sce_icc_u16_fn)(unsigned short *out);
typedef int (*sce_icc_u8_fn)(unsigned char *out);
static sce_icc_u32_fn p_sceKernelIccGetPowerOperatingTime = NULL;
static sce_icc_u32_fn p_sceKernelIccGetPowerNumberOfBootShutdown = NULL;
static sce_icc_u16_fn p_sceKernelIccGetThermalAlert = NULL;
static sce_icc_u8_fn  p_sceKernelIccGetPowerUpCause = NULL;
static int            sce_icc_get_resolve_attempted = 0;
static void resolve_sce_icc_get(void) {
    if (sce_icc_get_resolve_attempted) return;
    sce_icc_get_resolve_attempted = 1;
    p_sceKernelIccGetPowerOperatingTime = (sce_icc_u32_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccGetPowerOperatingTime");
    p_sceKernelIccGetPowerNumberOfBootShutdown = (sce_icc_u32_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccGetPowerNumberOfBootShutdown");
    p_sceKernelIccGetThermalAlert = (sce_icc_u16_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccGetThermalAlert");
    p_sceKernelIccGetPowerUpCause = (sce_icc_u8_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccGetPowerUpCause");
}
/* User service — libSceUserService. Initialise/Terminate are
 * idempotent; we call Initialize once on first user-list request and
 * leave the service open for subsequent calls. The list call returns
 * a fixed-size 16-int array — Sony's UI supports up to 16 users.
 *
 * sceUserServiceGetUserName takes (user_id, out_buf, out_buf_size)
 * and writes a UTF-8 null-terminated name. */
extern int sceUserServiceInitialize(void *params);
extern int sceUserServiceGetForegroundUser(int *user_id);
extern int sceUserServiceGetLoginUserIdList(int *id_list);
extern int sceUserServiceGetUserName(int user_id, char *name, size_t size);
extern int sceUserServiceSetUserName(int user_id, const char *name);
extern int sceUserServiceDestroyUser(int user_id);
extern int sceUserServiceGetInitialUser(int *user_id);
#define USER_SERVICE_MAX_USERS 16

/* App lifecycle — libSceSysCore exports. ApplicationGetProcs returns
 * the count of running apps via the in-out arg; the caller passes a
 * buffer + max_count and reads back the list. The proc struct shape
 * varies across firmware revisions but the first 4-byte field is
 * always the app_id. We treat each entry as opaque 24 bytes (a
 * documented-stable size from sceApplicationGetAppInfoByAppId
 * usage in the wild) and only extract app_id at offset 0.
 *
 * The Sony "GetProcs" type is `SceAppCallProcInfo` per psdevwiki.
 * For our purposes — list running app_ids so the user can
 * suspend/resume them — only the app_id field matters. */
/* libSceSysCore exports — also dlopen-resolved at first use (see the
 * libSceFsInternalForVsh comment for the rationale: compile-time
 * linkage of optional SPRX deps blocks the entire payload from
 * loading on FW where any one is missing). */
typedef int (*sce_app_simple_fn)(unsigned int app_id);
typedef int (*sce_app_get_procs_fn)(void *info_buf, int max_count,
                                     int *out_count);
static sce_app_simple_fn   p_sceApplicationSuspend    = NULL;
static sce_app_simple_fn   p_sceApplicationResume     = NULL;
static sce_app_simple_fn   p_sceApplicationKill       = NULL;
static sce_app_get_procs_fn p_sceApplicationGetProcs  = NULL;
static int sce_syscore_resolve_attempted = 0;
static int resolve_sce_syscore(void) {
    if (sce_syscore_resolve_attempted) {
        return (p_sceApplicationGetProcs || p_sceApplicationSuspend)
            ? 0 : -1;
    }
    sce_syscore_resolve_attempted = 1;
    void *h = dlopen("libSceSysCore.sprx", RTLD_LAZY);
    if (!h) return -1;
    p_sceApplicationSuspend  = (sce_app_simple_fn)
        dlsym(h, "sceApplicationSuspend");
    p_sceApplicationResume   = (sce_app_simple_fn)
        dlsym(h, "sceApplicationResume");
    p_sceApplicationKill     = (sce_app_simple_fn)
        dlsym(h, "sceApplicationKill");
    p_sceApplicationGetProcs = (sce_app_get_procs_fn)
        dlsym(h, "sceApplicationGetProcs");
    return (p_sceApplicationGetProcs || p_sceApplicationSuspend)
        ? 0 : -1;
}

/* Rich notification — libSceNotification. The JSON template format
 * is `{"requestId":N,"useIconImageUri":true,"requestId":1,
 *      "imageUri":"<url>","targetId":"NoTargetId","userId":N,
 *      "type":0,"messageType":N,"summary":"<title>","app":{
 *      "type":0},"icon":<NotificationIconType>,"messageBody":"<body>",
 *      ...}` — the docs are spotty, so we send a minimal shape and
 * Sony's daemon fills in defaults. */
/* sceNotificationSend — dlsym-resolved like the rest of the Sce*
 * surface (see reference_ps5_sdk_stub_vs_sprx note). SDK stub exports
 * it; on-console libSceNotification SPRX may not on every firmware,
 * and an eager-bound undef silently kills payload load. */
typedef int (*sce_notification_send_fn)(int target_user_id, int unknown_flag,
                                         const char *json_template);
static sce_notification_send_fn p_sceNotificationSend = NULL;
static int sce_notification_resolve_attempted = 0;
static void resolve_sce_notification(void) {
    if (sce_notification_resolve_attempted) return;
    sce_notification_resolve_attempted = 1;
    p_sceNotificationSend = (sce_notification_send_fn)
        dlsym(RTLD_DEFAULT, "sceNotificationSend");
}

/* Peripheral control + module enumeration — also dlsym-resolved for
 * the same reason as sceKernelIccGet*: the SDK stub exports them but
 * the actual on-PS5 SPRX may not, and a missing symbol kills load.
 * Even though these passed in field testing today (only ThermalAlert
 * was the assassin on this firmware), preemptive hardening avoids
 * repeating the same diagnostic cycle on the next FW that drops one. */
typedef int (*sce_icc_bd_fn)(int state);
typedef int (*sce_icc_usb_fn)(int port, int state);
typedef struct sce_module_info {
    size_t size;             /* sizeof(struct), Sony fills */
    char   name[256];
    int    type;             /* internal */
    int    pad;
    void  *base_addr;
    size_t code_size;
    void  *code_segment;
    /* … more fields, but we only need name + base + size … */
} sce_module_info_t;
typedef int (*sce_get_module_list_fn)(int *handle_list, int max_handles,
                                       int *out_count);
typedef int (*sce_get_module_info_fn)(int handle, sce_module_info_t *info);

static sce_icc_bd_fn         p_sceKernelIccControlBDPowerState  = NULL;
static sce_icc_usb_fn        p_sceKernelIccControlUSBPowerState = NULL;
static sce_get_module_list_fn p_sceKernelGetModuleList          = NULL;
static sce_get_module_info_fn p_sceKernelGetModuleInfo          = NULL;
static int                    sce_kernel_extras_resolve_attempted = 0;
static void resolve_sce_kernel_extras(void) {
    if (sce_kernel_extras_resolve_attempted) return;
    sce_kernel_extras_resolve_attempted = 1;
    p_sceKernelIccControlBDPowerState = (sce_icc_bd_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccControlBDPowerState");
    p_sceKernelIccControlUSBPowerState = (sce_icc_usb_fn)
        dlsym(RTLD_DEFAULT, "sceKernelIccControlUSBPowerState");
    p_sceKernelGetModuleList = (sce_get_module_list_fn)
        dlsym(RTLD_DEFAULT, "sceKernelGetModuleList");
    p_sceKernelGetModuleInfo = (sce_get_module_info_fn)
        dlsym(RTLD_DEFAULT, "sceKernelGetModuleInfo");
}

/* sceNetGetIfList — populates an array of network interface info
 * structs and returns the count. Per psdevwiki the struct shape is
 * 0x500 bytes, but only the first ~100 contain user-visible fields
 * (name, addresses, MAC). We treat each entry as 0x500 opaque bytes
 * and read the documented offsets.
 *
 * Resolved via dlopen at first use rather than compile-time linkage.
 * libSceNet isn't accessible to user-mode loaders on every PS5
 * firmware; a missing DT_NEEDED entry would cause rtld to refuse
 * to load the entire payload (no toast, no port bind, loader
 * silently rejects). With dlopen, missing → handler returns
 * `service_unavailable` and the rest of the payload keeps working. */
typedef int (*sce_net_init_fn)(void);
typedef int (*sce_net_get_if_list_fn)(void *list, int max_count, int *out_count);
static sce_net_get_if_list_fn p_sceNetGetIfList = NULL;
static int sce_net_resolve_attempted = 0;
static int resolve_sce_net(void) {
    if (sce_net_resolve_attempted) {
        return p_sceNetGetIfList ? 0 : -1;
    }
    sce_net_resolve_attempted = 1;
    void *h = dlopen("libSceNet.sprx", RTLD_LAZY);
    if (!h) return -1;
    sce_net_init_fn init = (sce_net_init_fn)dlsym(h, "sceNetInit");
    if (init) (void)init();  /* best-effort init */
    p_sceNetGetIfList = (sce_net_get_if_list_fn)dlsym(h, "sceNetGetIfList");
    return p_sceNetGetIfList ? 0 : -1;
}
#define NET_IF_ENTRY_BYTES 0x500
#define NET_IF_MAX_ENTRIES 16
/* sceSystemStateMgrEnterStandby is an alias inside the same library;
 * not all firmware revisions expose it directly. We dlsym at runtime
 * so a missing symbol degrades to "standby_unavailable" rather than
 * a load-time symbol error. */

/* Parse `{"action":"<name>"}` from a body. Returns one of the
 * SC_ACTION_* enum values, or -1 if the JSON is malformed / unknown. */
typedef enum {
    SC_ACTION_REBOOT = 0,
    SC_ACTION_SHUTDOWN = 1,
    SC_ACTION_STANDBY = 2,
    SC_ACTION_TICK = 3,
} system_control_action_t;

static int parse_system_control_action(const char *body, uint64_t body_len,
                                        system_control_action_t *out) {
    if (!body || body_len == 0 || body_len > 256) return -1;
    /* Look for `"action":"<value>"`. We don't need a full JSON parser
     * for one tiny field — substring search is enough and avoids
     * pulling in cJSON (not currently linked). */
    char buf[260];
    memcpy(buf, body, (size_t)body_len);
    buf[body_len] = '\0';
    const char *needle = "\"action\"";
    const char *p = strstr(buf, needle);
    if (!p) return -1;
    p += strlen(needle);
    while (*p == ' ' || *p == ':') p++;
    if (*p != '"') return -1;
    p++;
    /* p now points at the value start. Find closing quote. */
    const char *e = strchr(p, '"');
    if (!e) return -1;
    size_t vlen = (size_t)(e - p);
    if (vlen == 6 && strncmp(p, "reboot", 6) == 0) {
        *out = SC_ACTION_REBOOT;
        return 0;
    }
    if (vlen == 8 && strncmp(p, "shutdown", 8) == 0) {
        *out = SC_ACTION_SHUTDOWN;
        return 0;
    }
    if (vlen == 7 && strncmp(p, "standby", 7) == 0) {
        *out = SC_ACTION_STANDBY;
        return 0;
    }
    if (vlen == 4 && strncmp(p, "tick", 4) == 0) {
        *out = SC_ACTION_TICK;
        return 0;
    }
    return -1;
}

static int handle_system_control(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *body,
                                  uint64_t body_len) {
    if (!state) return -1;
    system_control_action_t action;
    if (parse_system_control_action(body, body_len, &action) != 0) {
        const char *err = "{\"ok\":false,\"err\":\"bad_action\"}";
        return send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK, 0,
                          trace_id, err, strlen(err));
    }

    /* Reply BEFORE invoking destructive APIs — reboot/shutdown will
     * tear down our network stack, so the client may never see the
     * ACK if we send after. The client treats "no ACK + drop" as
     * success per the protocol contract. */
    int rc = 0;
    int err_code = 0;
    const char *err_str = NULL;
    /* All four ACK bodies below previously had hand-counted lengths
     * that were off-by-one on 3 of 4 paths — reboot/shutdown/tick
     * dropped the closing `}`, which the client's serde_json parser
     * rejected with "EOF while parsing an object at line 1 column N".
     * The reboot/shutdown calls still went through (the API was
     * invoked after the truncated send), but the client surfaced a
     * spurious error to the user. Standby happened to be counted
     * correctly. Using strlen() on the literal keeps every path
     * honest and immunizes against future copies of this pattern. */
    switch (action) {
    case SC_ACTION_REBOOT: {
        /* Send ACK first, then call API. */
        const char *ack = "{\"ok\":true,\"action\":\"reboot\"}";
        rc = send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK, 0,
                        trace_id, ack, strlen(ack));
        sceSystemServiceRequestReboot();
        return rc;
    }
    case SC_ACTION_SHUTDOWN: {
        const char *ack = "{\"ok\":true,\"action\":\"shutdown\"}";
        rc = send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK, 0,
                        trace_id, ack, strlen(ack));
        /* sceSystemServiceRequestPowerOff() goes through the system's normal
         * power-button flow, which on PS5 RESPECTS the rest-mode setting — so
         * "Shutdown" commonly dropped the console into REST MODE instead of a
         * full power-off (user report). sceSystemStateMgrTurnOff() is the
         * direct turn-the-power-off call (same SystemStateMgr family as the
         * standby path's sceSystemStateMgrEnterStandby), which actually powers
         * the console off. dlsym it so a firmware that lacks the symbol
         * degrades to the old request-based behavior rather than failing.
         *
         * Called through an `(int)` pointer with arg 0: if the real symbol is
         * niladic the extra register is ignored; if it takes a mode/reason,
         * 0 is the safe "normal shutdown" default. Avoids passing a garbage
         * register the way a `(void)` cast would if the arity is non-zero. */
        void *h = dlsym(RTLD_DEFAULT, "sceSystemStateMgrTurnOff");
        if (h) {
            int (*turn_off)(int) = (int (*)(int))h;
            turn_off(0);
        } else {
            sceSystemServiceRequestPowerOff();
        }
        return rc;
    }
    case SC_ACTION_STANDBY: {
        /* sceSystemStateMgrEnterStandby is dlsym'd to handle FW where
         * the symbol moved or doesn't exist. */
        void *h = dlsym(RTLD_DEFAULT, "sceSystemStateMgrEnterStandby");
        if (!h) {
            err_str = "{\"ok\":false,\"err\":\"standby_unavailable\"}";
            return send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK,
                              0, trace_id, err_str, strlen(err_str));
        }
        int (*enter_standby)(void) = (int (*)(void))h;
        const char *ack = "{\"ok\":true,\"action\":\"standby\"}";
        rc = send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK, 0,
                        trace_id, ack, strlen(ack));
        enter_standby();
        return rc;
    }
    case SC_ACTION_TICK:
        /* Tick is non-destructive; we can ACK after. */
        err_code = sceSystemServicePowerTick();
        if (err_code == 0) {
            const char *ack = "{\"ok\":true,\"action\":\"tick\"}";
            return send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK,
                              0, trace_id, ack, strlen(ack));
        }
        {
            char buf[128];
            int n = snprintf(buf, sizeof(buf),
                             "{\"ok\":false,\"err\":\"power_tick_failed\","
                             "\"code\":%d}", err_code);
            return send_frame(client_fd, FTX2_FRAME_SYSTEM_CONTROL_ACK,
                              0, trace_id, buf, (size_t)n);
        }
    }
    /* Unreachable, but quiet the compiler. */
    return -1;
}

/* ── Power telemetry handler ─────────────────────────────────────────── */

static int handle_power_telemetry(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id) {
    if (!state) return -1;
    /* Each ICC call is independent — failures are non-fatal so the
     * caller still sees whatever did succeed. We emit `<key>=<value>`
     * for successful reads and `<key>=err` for the others. */
    char body[512];
    int n = 0;
    unsigned int op_secs = 0;
    unsigned int boot_cycles = 0;
    unsigned short thermal_flags = 0;
    unsigned char power_up_cause = 0;
    resolve_sce_icc_get();
    int rc_op   = p_sceKernelIccGetPowerOperatingTime
                    ? p_sceKernelIccGetPowerOperatingTime(&op_secs) : -1;
    int rc_boot = p_sceKernelIccGetPowerNumberOfBootShutdown
                    ? p_sceKernelIccGetPowerNumberOfBootShutdown(&boot_cycles) : -1;
    int rc_therm = p_sceKernelIccGetThermalAlert
                    ? p_sceKernelIccGetThermalAlert(&thermal_flags) : -1;
    int rc_pwc  = p_sceKernelIccGetPowerUpCause
                    ? p_sceKernelIccGetPowerUpCause(&power_up_cause) : -1;
    if (rc_op == 0) {
        n += snprintf(body + n, sizeof(body) - n,
                      "operating_seconds=%u\n", op_secs);
    } else {
        n += snprintf(body + n, sizeof(body) - n,
                      "operating_seconds=err\n");
    }
    if (rc_boot == 0) {
        n += snprintf(body + n, sizeof(body) - n,
                      "boot_cycles=%u\n", boot_cycles);
    } else {
        n += snprintf(body + n, sizeof(body) - n,
                      "boot_cycles=err\n");
    }
    if (rc_therm == 0) {
        n += snprintf(body + n, sizeof(body) - n,
                      "thermal_alert_flags=%u\n", (unsigned)thermal_flags);
    } else {
        n += snprintf(body + n, sizeof(body) - n,
                      "thermal_alert_flags=err\n");
    }
    if (rc_pwc == 0) {
        n += snprintf(body + n, sizeof(body) - n,
                      "power_up_cause=%u\n", (unsigned)power_up_cause);
    } else {
        n += snprintf(body + n, sizeof(body) - n,
                      "power_up_cause=err\n");
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_POWER_TELEMETRY_ACK, 0,
                      trace_id, body, (uint64_t)n);
}

/* ── User account enumeration ────────────────────────────────────────── */

static int handle_user_list(runtime_state_t *state, int client_fd,
                            uint64_t trace_id) {
    if (!state) return -1;
    /* Init is idempotent — Sony's API is documented to no-op on a
     * second call. We don't track a "first call done" flag because
     * the cost is negligible and statelessness avoids cross-thread
     * locking concerns. */
    sceUserServiceInitialize(NULL);
    int foreground = -1;
    int rc_fg = sceUserServiceGetForegroundUser(&foreground);
    int ids[USER_SERVICE_MAX_USERS];
    /* Sony fills unused slots with -1; iterate until we hit one. */
    for (int i = 0; i < USER_SERVICE_MAX_USERS; i++) ids[i] = -1;
    int rc_list = sceUserServiceGetLoginUserIdList(ids);
    /* Build response JSON. Bounded buffer — 16 users × ~80 bytes per
     * entry max = ~1.3 KB. 4 KB gives generous headroom. */
    char body[4096];
    int n = 0;
    n += snprintf(body + n, sizeof(body) - n,
                  "{\"foreground\":%d,\"err_fg\":%d,\"err_list\":%d,\"users\":[",
                  rc_fg == 0 ? foreground : -1, rc_fg, rc_list);
    int wrote_one = 0;
    for (int i = 0; i < USER_SERVICE_MAX_USERS; i++) {
        if (ids[i] < 0) continue;
        char name[64];
        name[0] = '\0';
        int rc_name = sceUserServiceGetUserName(ids[i], name, sizeof(name));
        if (n >= (int)sizeof(body) - 100) break;
        if (wrote_one) {
            body[n++] = ',';
        }
        wrote_one = 1;
        char esc[128];
        json_escape_into(name, esc, sizeof(esc));
        n += snprintf(body + n, sizeof(body) - n,
                      "{\"id\":%d,\"name\":\"%s\",\"foreground\":%s,\"err_name\":%d}",
                      ids[i], esc,
                      ids[i] == foreground ? "true" : "false", rc_name);
    }
    if (n < (int)sizeof(body) - 2) {
        body[n++] = ']';
        body[n++] = '}';
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_USER_LIST_ACK, 0, trace_id,
                      body, (uint64_t)n);
}

/* ── User create / delete ──────────────────────────────────────────────
 * sceUserServiceCreateUser allocates a new local user account and
 * returns its user_id. The initial name is set via SetUserName after
 * creation. sceUserServiceDeleteUser removes the account; Sony cleans
 * up home dir content but we offer an optional pre-wipe of savedata. */

static int handle_user_create(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char name[64] = {0};
    extract_json_string_field(body, "name", name, sizeof(name));
    int rc = -1;
    int new_uid = -1;
    const char *err_msg = "";
    if (name[0]) {
        sceUserServiceInitialize(NULL);
        pthread_mutex_lock(&sony_api_lock);
        int raw_uid = -1;
        int init_rc = sceUserServiceGetInitialUser(&raw_uid);
        if (init_rc == 0 && raw_uid >= 0) {
            int name_rc = sceUserServiceSetUserName(raw_uid, name);
            if (name_rc != 0) {
                err_msg = "user found but SetUserName failed";
            }
            new_uid = raw_uid;
            rc = 0;
        } else {
            err_msg = "sceUserServiceGetInitialUser failed";
        }
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
    } else {
        err_msg = "empty name";
    }
    char nesc[128];
    json_escape_into(name, nesc, sizeof(nesc));
    char eesc[256];
    json_escape_into(err_msg, eesc, sizeof(eesc));
    char resp[384];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"uid\":%d,\"name\":\"%s\",\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", new_uid, nesc, eesc);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_USER_CREATE_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

static int handle_user_delete(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body) {
    if (!state) return -1;
    int uid = (int)extract_json_uint64_field(body, "uid");
    int wipe_saves = (int)extract_json_uint64_field(body, "wipe_saves");
    int rc = -1;
    const char *err_msg = "";
    if (uid > 0) {
        /* Optional: wipe savedata before deleting the user account so
         * Sony's cleanup doesn't leave orphaned sealed saves. */
        if (wipe_saves) {
            char sd_path[512];
            snprintf(sd_path, sizeof(sd_path),
                     "/user/home/%d/savedata", uid);
            remove_recursive_path(sd_path, NULL, NULL);
            snprintf(sd_path, sizeof(sd_path),
                     "/user/home/%d/savedata_prospero", uid);
            remove_recursive_path(sd_path, NULL, NULL);
        }
        sceUserServiceInitialize(NULL);
        pthread_mutex_lock(&sony_api_lock);
        int del_rc = sceUserServiceDestroyUser(uid);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
        if (del_rc == 0) {
            rc = 0;
        } else {
            err_msg = "sceUserServiceDestroyUser failed";
        }
    } else {
        err_msg = "invalid uid";
    }
    char eesc[256];
    json_escape_into(err_msg, eesc, sizeof(eesc));
    char resp[256];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"uid\":%d,\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", uid, eesc);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_USER_DELETE_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

/* ── Backup & restore ────────────────────────────────────────────────── */

static int handle_backup_snapshot(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    char path[512] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    extract_json_string_field(body, "path", path, sizeof(path));
    int64_t ts = 0;
    int files = 0;
    uint64_t bytes = 0;
    const char *err = "";
    int rc = -1;
    if (tag[0] && path[0]) {
        rc = backup_snapshot(tag, path, &ts, &files, &bytes);
        if (rc != 0) err = "snapshot failed (source not found or empty)";
    } else {
        err = "missing tag or path";
    }
    char resp[512];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"tag\":\"%s\",\"timestamp\":%lld,\"files\":%d,"
        "\"bytes\":%llu,\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", tag, (long long)ts, files,
        (unsigned long long)bytes, err);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_BACKUP_SNAPSHOT_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

static int handle_backup_list(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    char *buf = malloc(32768);
    if (!buf) {
        const char *e = "{\"ok\":false,\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_BACKUP_LIST_ACK, 0, trace_id,
                          e, strlen(e));
    }
    size_t written = 0;
    backup_list(tag[0] ? tag : NULL, buf, 32768, &written);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = send_frame(client_fd, FTX2_FRAME_BACKUP_LIST_ACK, 0, trace_id,
                        buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_backup_restore(runtime_state_t *state, int client_fd,
                                 uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    int64_t ts = (int64_t)extract_json_uint64_field(body, "timestamp");
    int restored = 0;
    const char *err = "";
    int rc = -1;
    if (tag[0] && ts > 0) {
        rc = backup_restore(tag, ts, &restored);
        if (rc != 0) err = "snapshot not found or restore failed";
    } else {
        err = "missing tag or timestamp";
    }
    char resp[256];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"tag\":\"%s\",\"restored\":%d,\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", tag, restored, err);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_BACKUP_RESTORE_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

static int handle_backup_delete(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    int64_t ts = (int64_t)extract_json_uint64_field(body, "timestamp");
    const char *err = "";
    int rc = -1;
    if (tag[0] && ts > 0) {
        rc = backup_delete(tag, ts);
        if (rc != 0) err = "snapshot not found";
    } else {
        err = "missing tag or timestamp";
    }
    char resp[192];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"tag\":\"%s\",\"timestamp\":%lld,\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", tag, (long long)ts, err);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_BACKUP_DELETE_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

/* ── Remote Play handlers ────────────────────────────────────────────── */
static int handle_remoteplay_request(runtime_state_t *state, int client_fd,
                                     uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char acct[64] = {0};
    extract_json_string_field(body, "manual_account_id", acct, sizeof(acct));
    int rc = remoteplay_request(acct[0] ? acct : NULL);
    char resp[64];
    int len = snprintf(resp, sizeof(resp), "{\"ok\":%s}",
                       rc == 0 ? "true" : "false");
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_REMOTEPLAY_STATUS, 0, trace_id,
                      resp, (uint64_t)len);
}

static int handle_remoteplay_status(runtime_state_t *state, int client_fd,
                                    uint64_t trace_id) {
    if (!state) return -1;
    char buf[512];
    int rc = remoteplay_get_status(buf, sizeof(buf));
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    if (rc != 0) {
        const char *e = "{\"state\":\"failed\"}";
        return send_frame(client_fd, FTX2_FRAME_REMOTEPLAY_STATUS, 0,
                          trace_id, e, strlen(e));
    }
    return send_frame(client_fd, FTX2_FRAME_REMOTEPLAY_STATUS, 0, trace_id,
                      buf, strlen(buf));
}

static int handle_remoteplay_cancel(runtime_state_t *state, int client_fd,
                                    uint64_t trace_id) {
    if (!state) return -1;
    remoteplay_cancel();
    const char *resp = "{\"ok\":true}";
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_REMOTEPLAY_CANCEL_ACK, 0, trace_id,
                      resp, strlen(resp));
}

/* ── Fan curve handler ───────────────────────────────────────────────── */
static int handle_fan_curve_set(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *body) {
    if (!state) return -1;
    char err[256] = {0};
    int rc = fan_curve_set(body, err, sizeof(err));
    char resp[384];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"err\":\"%s\"}",
        rc == 0 ? "true" : "false", err);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_HW_FAN_CURVE_SET_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

/* ── Notifications handler ───────────────────────────────────────────── */
static int handle_notif_list(runtime_state_t *state, int client_fd,
                             uint64_t trace_id, const char *body) {
    if (!state) return -1;
    uint64_t since = extract_json_uint64_field(body, "since_seq");
    char *buf = malloc(32768);
    if (!buf) {
        const char *e = "{\"notifications\":[]}";
        return send_frame(client_fd, FTX2_FRAME_NOTIF_LIST_ACK, 0, trace_id,
                          e, strlen(e));
    }
    size_t written = 0;
    notif_list(since, buf, 32768, &written);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = send_frame(client_fd, FTX2_FRAME_NOTIF_LIST_ACK, 0, trace_id,
                        buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Notification send handler ──────────────────────────────────────── */
static int handle_notif_send(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body) {
    if (!state) return -1;
    if (!body) {
        const char *err = "{\"ok\":false,\"err\":\"body_required\"}";
        return send_frame(client_fd, FTX2_FRAME_NOTIF_SEND_ACK, 0, trace_id,
                          err, strlen(err));
    }
    char msg[512] = {0};
    extract_json_string_field(body, "msg", msg, sizeof(msg));
    if (msg[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"msg_required\"}";
        return send_frame(client_fd, FTX2_FRAME_NOTIF_SEND_ACK, 0, trace_id,
                          err, strlen(err));
    }
    int level = (int)extract_json_uint64_field(body, "level");
    int rc = notif_send(msg, level);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *resp = (rc == 0)
        ? "{\"ok\":true}"
        : "{\"ok\":false,\"err\":\"send_failed\"}";
    return send_frame(client_fd, FTX2_FRAME_NOTIF_SEND_ACK, 0, trace_id,
                      resp, strlen(resp));
}

/* ── Fan curve get handler ───────────────────────────────────────────── */
static int handle_fan_curve_get(runtime_state_t *state, int client_fd,
                                 uint64_t trace_id) {
    if (!state) return -1;
    char buf[4096];
    int rc = fan_curve_get(buf, sizeof(buf));
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    if (rc != 0) {
        const char *e = "{\"points\":[]}";
        return send_frame(client_fd, FTX2_FRAME_HW_FAN_CURVE_GET_ACK, 0,
                          trace_id, e, strlen(e));
    }
    return send_frame(client_fd, FTX2_FRAME_HW_FAN_CURVE_GET_ACK, 0,
                      trace_id, buf, strlen(buf));
}

/* ── Save-data listing ───────────────────────────────────────────────── */

/* Walk a savedata root for one user, appending JSON entries to `body`.
 * `root` is e.g. "/user/home/<uid>/savedata_prospero" or
 * "/user/home/<uid>/savedata". Each child dir is a title_id; we record
 * the dir path + total size + mtime. Per-title descents are NOT done —
 * that data lives inside each game's savedata folder and Sony's PFS
 * encryption hides it from us anyway.
 *
 * Returns the number of entries written; -1 on error (caller handles). */
static int append_saves_for_root(const char *root, int user_id, int kind_is_ps4,
                                  char *body, int *n, int cap, int already_wrote) {
    DIR *dp = opendir(root);
    if (!dp) return 0;  /* Missing root is fine — user just hasn't used PS4 saves yet. */
    int count = 0;
    int wrote_one = already_wrote;
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.') continue;
        char path[1024];
        snprintf(path, sizeof(path), "%s/%s", root, e->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        if (!S_ISDIR(st.st_mode)) continue;
        /* Compute dir size by a TWO-level scan (immediate files + one level
         * of subdirectories). Saves keep metadata under `sce_sys/` (icons,
         * param.sfo, sealed keys), which a single-level scan skipped — so the
         * reported size understated the save (and the resulting backup zip).
         * One level of descent covers sce_sys without unbounded recursion;
         * still "approximate disk usage", just no longer missing sce_sys. */
        long long total = 0;
        DIR *cd = opendir(path);
        if (cd) {
            struct dirent *fe;
            while ((fe = readdir(cd)) != NULL) {
                if (fe->d_name[0] == '.') continue;
                char child[1500];
                snprintf(child, sizeof(child), "%s/%s", path, fe->d_name);
                struct stat fst;
                if (stat(child, &fst) != 0) continue;
                if (S_ISREG(fst.st_mode)) {
                    total += fst.st_size;
                } else if (S_ISDIR(fst.st_mode)) {
                    /* Descend one level (e.g. sce_sys/) — files only, no
                     * further recursion. */
                    DIR *gd = opendir(child);
                    if (gd) {
                        struct dirent *ge;
                        while ((ge = readdir(gd)) != NULL) {
                            if (ge->d_name[0] == '.') continue;
                            char gchild[2100];
                            snprintf(gchild, sizeof(gchild), "%s/%s",
                                     child, ge->d_name);
                            struct stat gst;
                            if (stat(gchild, &gst) == 0 && S_ISREG(gst.st_mode))
                                total += gst.st_size;
                        }
                        closedir(gd);
                    }
                }
            }
            closedir(cd);
        }
        char esc_title[512];
        char esc_path[2048];
        json_escape_into(e->d_name, esc_title, sizeof(esc_title));
        json_escape_into(path, esc_path, sizeof(esc_path));
        if (*n >= cap - 2800) break;
        if (wrote_one) {
            body[(*n)++] = ',';
        }
        wrote_one = 1;
        *n += snprintf(body + *n, cap - *n,
                       "{\"title_id\":\"%s\",\"user_id\":%d,\"path\":\"%s\","
                       "\"size\":%lld,\"mtime\":%lld,\"kind\":\"%s\"}",
                       esc_title, user_id, esc_path, total,
                       (long long)st.st_mtime,
                       kind_is_ps4 ? "ps4" : "ps5");
        count++;
    }
    closedir(dp);
    return count;
}

static int handle_list_saves(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body, uint64_t body_len) {
    if (!state) return -1;
    /* Optional `{"user_id":N}` body filters to one user; else walk all
     * user dirs under /user/home. */
    int filter_uid = 0;
    if (body && body_len > 0 && body_len < 256) {
        char buf[260];
        memcpy(buf, body, (size_t)body_len);
        buf[body_len] = '\0';
        const char *p = strstr(buf, "\"user_id\"");
        if (p) {
            p += strlen("\"user_id\"");
            while (*p == ' ' || *p == ':') p++;
            filter_uid = atoi(p);
        }
    }

    /* Response body is large — saves can number in the hundreds. 64 KB
     * is comfortable; the ~100-byte-per-entry budget gives room for
     * ~600 entries before truncation. */
    char *resp = malloc(64 * 1024);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_LIST_SAVES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"saves\":[");
    int wrote_one = 0;

    DIR *home = opendir("/user/home");
    if (home) {
        struct dirent *uent;
        while ((uent = readdir(home)) != NULL) {
            if (uent->d_name[0] == '.') continue;
            /* PS5 user dirs are 8-hex-digit IDs (e.g. "179a0cd8"), NOT
             * decimal integers as the earlier `atoi(d_name)` assumed.
             * `atoi("179a0cd8")` stopped at the 'a' and returned 179,
             * then snprintf built `/user/home/179/...` which doesn't
             * exist — Saves screen always came back empty.
             *
             * Parse hex into a u32 for the JSON `user_id` field, but
             * always build paths from the raw directory name so non-
             * numeric or edge-case names still resolve. */
            const char *raw_name = uent->d_name;
            char *endp = NULL;
            unsigned long uid_u32 = strtoul(raw_name, &endp, 16);
            if (!endp || *endp != '\0' || uid_u32 == 0) continue;
            int uid_for_json = (int)(uid_u32 & 0x7FFFFFFFu);
            if (filter_uid != 0 && uid_for_json != filter_uid) continue;
            char ps5_root[256];
            char ps4_root[256];
            snprintf(ps5_root, sizeof(ps5_root),
                     "/user/home/%s/savedata_prospero", raw_name);
            snprintf(ps4_root, sizeof(ps4_root),
                     "/user/home/%s/savedata", raw_name);
            int c = append_saves_for_root(ps5_root, uid_for_json, 0,
                                          resp, &n, cap, wrote_one);
            if (c > 0) wrote_one = 1;
            c = append_saves_for_root(ps4_root, uid_for_json, 1,
                                      resp, &n, cap, wrote_one);
            if (c > 0) wrote_one = 1;
            if (n >= cap - 100) break;
        }
        closedir(home);
    }
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc = send_frame(client_fd, FTX2_FRAME_LIST_SAVES_ACK, 0, trace_id,
                        resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* ── Screenshot listing ──────────────────────────────────────────────── */

/* A bounded set of screenshot "stems" (basename with trailing image
 * extensions stripped) used to suppress thumbnail duplicates. Both the
 * full-res original `<name>.jxr` and Sony's doubled-suffix thumbnail
 * `<name>.jxr.jxr` collapse to the same stem `<name>`, so once an
 * original is listed its thumbnail can be recognised and skipped.
 * Backing store is a single malloc'd block; membership is a linear scan
 * (screenshot counts are in the hundreds, bounded by the 64 KiB response
 * buffer, so O(n²) here is trivial). If allocation fails the set degrades
 * to empty — dedup is skipped but listing still works. */
#define SS_STEM_MAX  64
typedef struct {
    char (*names)[SS_STEM_MAX];
    int count;
    int cap;
} ss_seen_t;

static int ss_seen_has(const ss_seen_t *s, const char *stem) {
    for (int i = 0; i < s->count; i++) {
        if (strcmp(s->names[i], stem) == 0) return 1;
    }
    return 0;
}

static void ss_seen_add(ss_seen_t *s, const char *stem) {
    if (s->count >= s->cap) return;  /* bounded; stop recording past cap */
    snprintf(s->names[s->count], SS_STEM_MAX, "%s", stem);
    s->count++;
}

/* Strip trailing image extensions to get a stable identity for a shot.
 * `<name>.jxr` and `<name>.jxr.jxr` both reduce to `<name>`. */
static void ss_stem(const char *fname, char *out, size_t out_sz) {
    snprintf(out, out_sz, "%s", fname);
    for (;;) {
        char *dot = strrchr(out, '.');
        if (!dot) break;
        if (strcmp(dot, ".jxr") == 0 ||
            strcmp(dot, ".jpg") == 0 ||
            strcmp(dot, ".jpeg") == 0) {
            *dot = '\0';
            continue;
        }
        break;
    }
}

/* Recursively walk dir up to `depth_left` levels deep; for each image
 * file append a JSON entry to the buffer. Sony stores PS5 screenshots
 * as JPEG XR (`.jxr`) at:
 *   /user/av_contents/photo/<userId>/<userId>/<batch>/<file>.jxr       (full-res, ~1 MiB)
 *   /user/av_contents/thumbnails/photo/<userId>/<userId>/<batch>/<file>.jxr.jxr  (thumbnail)
 * with `.dat` (raw) and `.meta` sidecars next to each.
 *
 * Earlier this filter accepted only `.jpg`/`.jpeg`, which matched 0
 * files on actual PS5 firmware — the Screenshots tab showed empty even
 * though the user had screenshots. We accept .jxr too now; the client
 * lists metadata only (no inline rendering) so the lack of a JXR
 * decoder in the browser doesn't matter — the user downloads the
 * original .jxr and opens it in a viewer that supports JPEG XR.
 *
 * `seen` tracks stems already emitted. The full-res tree is walked first
 * (is_thumb=0, recording every stem); the thumbnail tree second
 * (is_thumb=1, skipping any stem already seen) so each shot appears once,
 * with orphan thumbnails (original deleted) still surfacing as a fallback. */
static int walk_screenshots(const char *root, int depth_left,
                             char *body, int *n, int cap, int *wrote_one,
                             ss_seen_t *seen, int is_thumb) {
    if (depth_left <= 0) return 0;
    DIR *dp = opendir(root);
    if (!dp) return 0;
    int count = 0;
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.') continue;
        char path[1024];
        snprintf(path, sizeof(path), "%s/%s", root, e->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            count += walk_screenshots(path, depth_left - 1, body, n, cap,
                                      wrote_one, seen, is_thumb);
            continue;
        }
        /* Accept .jxr (Sony's native format), and legacy .jpg/.jpeg
         * (in case a future FW or sidecar starts writing them). The
         * `.jxr.jxr` thumbnail double-suffix is matched by the .jxr
         * extension check since strrchr finds the trailing one. Skip
         * .dat/.meta sidecars and anything else. */
        const char *ext = strrchr(e->d_name, '.');
        if (!ext) continue;
        if (strcmp(ext, ".jxr") != 0 &&
            strcmp(ext, ".jpg") != 0 &&
            strcmp(ext, ".jpeg") != 0) continue;
        /* Suppress a thumbnail whose full-res original was already
         * listed; record every emitted stem so later entries dedup. */
        char stem[SS_STEM_MAX];
        ss_stem(e->d_name, stem, sizeof(stem));
        if (is_thumb && ss_seen_has(seen, stem)) continue;
        char esc_path[2048];
        json_escape_into(path, esc_path, sizeof(esc_path));
        if (*n >= cap - 2300) break;
        if (*wrote_one) {
            body[(*n)++] = ',';
        }
        *wrote_one = 1;
        *n += snprintf(body + *n, cap - *n,
                       "{\"path\":\"%s\",\"size\":%lld,\"mtime\":%lld}",
                       esc_path, (long long)st.st_size, (long long)st.st_mtime);
        ss_seen_add(seen, stem);
        count++;
    }
    closedir(dp);
    return count;
}

/* Recursively walk `root` up to `depth_left` levels; append a JSON entry
 * for each video-clip file found. PS5 stores gameplay video clips under
 *   /user/av_contents/video/<userId>/<userId>/<batch>/<file>.webm
 * (WebM is the native container on current firmware; older/imported
 * clips may be .mp4). Unlike screenshots there is no separate thumbnail
 * tree to dedup, so this is a plain flat walk — same JSON shape
 * (path/size/mtime) as walk_screenshots so the client reuses the row
 * renderer and the generic transfer-download path.
 *
 * NB (needs FW confirmation): the exact av_contents/video path + the
 * .webm/.mp4 extension set are the documented layout but haven't been
 * verified against a live console in-house — mirror of the screenshot
 * bug where a wrong extension listed 0 files. If the Clips tab shows
 * empty on hardware with clips present, adjust the extension filter /
 * root here. */
static int walk_videos(const char *root, int depth_left,
                       char *body, int *n, int cap, int *wrote_one) {
    if (depth_left <= 0) return 0;
    DIR *dp = opendir(root);
    if (!dp) return 0;
    int count = 0;
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.') continue;
        char path[1024];
        snprintf(path, sizeof(path), "%s/%s", root, e->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            count += walk_videos(path, depth_left - 1, body, n, cap, wrote_one);
            continue;
        }
        const char *ext = strrchr(e->d_name, '.');
        if (!ext) continue;
        if (strcmp(ext, ".webm") != 0 &&
            strcmp(ext, ".mp4") != 0 &&
            strcmp(ext, ".mov") != 0) continue;
        char esc_path[2048];
        json_escape_into(path, esc_path, sizeof(esc_path));
        if (*n >= cap - 2300) break;
        if (*wrote_one) {
            body[(*n)++] = ',';
        }
        *wrote_one = 1;
        *n += snprintf(body + *n, cap - *n,
                       "{\"path\":\"%s\",\"size\":%lld,\"mtime\":%lld}",
                       esc_path, (long long)st.st_size, (long long)st.st_mtime);
        count++;
    }
    closedir(dp);
    return count;
}

/* ── Filesystem search index ──────────────────────────────────────────
 *
 * Build an in-memory index of every regular file under a set of roots,
 * then offer wildcard + size-filter searches against it.
 *
 * State is global to this translation unit: g_index_phase + g_index_lock
 * + g_index_entries. One indexing operation at a time; the renderer is
 * expected to call INDEX_STATUS to wait for completion before searching.
 *
 * Bounds: 64-byte (path) × 200K typical = 12-13 MB. Allocated via
 * realloc-with-doubling so the working set stays close to actual file
 * count. */

typedef struct {
    char *path;  /* malloc'd, null-terminated */
    long long size;
} index_entry_t;

/* Hard ceilings on the file-search index. The walk roots are
 * client-supplied (INDEX_START body) and the walk stores one strdup'd
 * path per regular file found, so on a populated game drive (the
 * `validate-xl` profile alone is 200k files) an uncapped index would
 * grow to hundreds of MB and OOM the payload — the one growable
 * structure in the server that lacked a cap. We bound BOTH the entry
 * count and the cumulative bytes (paths vary in length, so a count cap
 * alone doesn't bound memory); whichever trips first stops the walk and
 * marks the index truncated. ~200k entries + 32 MiB of path text is a
 * generous ceiling for a search index while staying well within the
 * payload's memory budget. */
#define INDEX_MAX_ENTRIES 200000u
#define INDEX_MAX_BYTES   (32u * 1024u * 1024u)

static pthread_mutex_t g_index_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_index_phase = 0;  /* 0=idle, 1=building, 2=ready */
static index_entry_t *g_index_entries = NULL;
static size_t g_index_count = 0;
static size_t g_index_cap = 0;
static size_t g_index_bytes = 0;     /* approx heap used by entries + paths */
static int g_index_truncated = 0;    /* hit INDEX_MAX_* and stopped early */
static int g_index_cancel = 0;
static time_t g_index_started_at = 0;
static time_t g_index_completed_at = 0;
static pthread_t g_index_thread;
static int g_index_thread_alive = 0;

static void index_clear_locked(void) {
    if (g_index_entries) {
        for (size_t i = 0; i < g_index_count; i++) {
            free(g_index_entries[i].path);
        }
        free(g_index_entries);
        g_index_entries = NULL;
    }
    g_index_count = 0;
    g_index_cap = 0;
    g_index_bytes = 0;
    g_index_truncated = 0;
}

/* Returns 0 if the entry was stored, -1 if the index is full (caller
 * stops walking). On a full index we set g_index_truncated rather than
 * growing without limit. */
static int index_push_locked(const char *path, long long size) {
    if (g_index_count >= INDEX_MAX_ENTRIES || g_index_bytes >= INDEX_MAX_BYTES) {
        g_index_truncated = 1;
        return -1;
    }
    if (g_index_count >= g_index_cap) {
        size_t new_cap = g_index_cap == 0 ? 4096 : g_index_cap * 2;
        if (new_cap > INDEX_MAX_ENTRIES) new_cap = INDEX_MAX_ENTRIES;
        index_entry_t *next =
            realloc(g_index_entries, new_cap * sizeof(index_entry_t));
        if (!next) return -1;
        g_index_entries = next;
        g_index_cap = new_cap;
    }
    char *dup = strdup(path);
    if (!dup) return -1;
    g_index_entries[g_index_count].path = dup;
    g_index_entries[g_index_count].size = size;
    g_index_count++;
    g_index_bytes += strlen(dup) + 1 + sizeof(index_entry_t);
    return 0;
}

static void index_walk(const char *root, int depth) {
    if (depth > 10) return;
    pthread_mutex_lock(&g_index_lock);
    int stop = g_index_cancel || g_index_truncated;
    pthread_mutex_unlock(&g_index_lock);
    if (stop) return;
    DIR *dp = opendir(root);
    if (!dp) return;
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.') continue;
        char path[1024];
        snprintf(path, sizeof(path), "%s/%s", root, e->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            index_walk(path, depth + 1);
        } else if (S_ISREG(st.st_mode)) {
            pthread_mutex_lock(&g_index_lock);
            int rc = index_push_locked(path, (long long)st.st_size);
            pthread_mutex_unlock(&g_index_lock);
            /* Index full (cap or byte budget hit) — stop this directory
             * scan; the truncated flag makes deeper recursions bail too. */
            if (rc != 0) break;
        }
    }
    closedir(dp);
}

typedef struct {
    /* Up to 8 roots to walk in this index build. */
    char roots[8][256];
    int root_count;
} index_thread_args_t;

static void *index_thread_fn(void *arg) {
    index_thread_args_t *args = (index_thread_args_t *)arg;
    for (int i = 0; i < args->root_count; i++) {
        index_walk(args->roots[i], 0);
    }
    pthread_mutex_lock(&g_index_lock);
    g_index_phase = 2;
    g_index_completed_at = time(NULL);
    g_index_thread_alive = 0;
    pthread_mutex_unlock(&g_index_lock);
    free(args);
    return NULL;
}

/* Tiny glob match: `*` matches any sequence, `?` matches one char.
 * Both are case-insensitive. Recursive — fine for typical patterns
 * with one or two wildcards; bounded by call depth (max 16).
 * Returns 1 on match, 0 on miss. */
static int glob_match(const char *pat, const char *str, int depth) {
    if (depth > 16) return 0;
    while (*pat) {
        if (*pat == '*') {
            pat++;
            if (!*pat) return 1;
            while (*str) {
                if (glob_match(pat, str, depth + 1)) return 1;
                str++;
            }
            return 0;
        }
        if (*pat == '?') {
            if (!*str) return 0;
            pat++;
            str++;
            continue;
        }
        char a = *pat;
        char b = *str;
        if (a >= 'A' && a <= 'Z') a += 32;
        if (b >= 'A' && b <= 'Z') b += 32;
        if (a != b) return 0;
        pat++;
        str++;
    }
    return *str == '\0';
}

static int handle_index_start(runtime_state_t *state, int client_fd,
                               uint64_t trace_id, const char *body, uint64_t body_len) {
    if (!state) return -1;
    pthread_mutex_lock(&g_index_lock);
    if (g_index_phase == 1) {
        pthread_mutex_unlock(&g_index_lock);
        const char *err = "{\"started\":false,\"err\":\"already_building\"}";
        return send_frame(client_fd, FTX2_FRAME_INDEX_START_ACK, 0,
                          trace_id, err, strlen(err));
    }
    index_clear_locked();
    g_index_phase = 1;
    g_index_started_at = time(NULL);
    g_index_completed_at = 0;
    g_index_cancel = 0;
    pthread_mutex_unlock(&g_index_lock);

    /* Parse roots from `{"roots":["/a","/b"]}`. Cheap string search
     * rather than full JSON — body is small, format is fixed. */
    index_thread_args_t *args = calloc(1, sizeof(*args));
    if (!args) {
        pthread_mutex_lock(&g_index_lock);
        g_index_phase = 0;
        pthread_mutex_unlock(&g_index_lock);
        const char *err = "{\"started\":false,\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_INDEX_START_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (body && body_len > 0 && body_len < 1024) {
        char tmp[1024];
        memcpy(tmp, body, body_len);
        tmp[body_len] = '\0';
        const char *p = tmp;
        while ((p = strchr(p, '"')) != NULL && args->root_count < 8) {
            p++;
            if (*p == '/') {
                const char *e = strchr(p, '"');
                if (!e) break;
                size_t len = (size_t)(e - p);
                if (len < sizeof(args->roots[0])) {
                    memcpy(args->roots[args->root_count], p, len);
                    args->roots[args->root_count][len] = '\0';
                    args->root_count++;
                }
                p = e + 1;
            }
        }
    }
    if (args->root_count == 0) {
        /* Sensible default. */
        strcpy(args->roots[0], "/user");
        strcpy(args->roots[1], "/data");
        args->root_count = 2;
    }
    pthread_mutex_lock(&g_index_lock);
    g_index_thread_alive = 1;
    pthread_mutex_unlock(&g_index_lock);
    if (pthread_create(&g_index_thread, NULL, index_thread_fn, args) != 0) {
        free(args);
        pthread_mutex_lock(&g_index_lock);
        g_index_phase = 0;
        g_index_thread_alive = 0;
        pthread_mutex_unlock(&g_index_lock);
        const char *err = "{\"started\":false,\"err\":\"thread_create\"}";
        return send_frame(client_fd, FTX2_FRAME_INDEX_START_ACK, 0,
                          trace_id, err, strlen(err));
    }
    pthread_detach(g_index_thread);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *ok = "{\"started\":true}";
    return send_frame(client_fd, FTX2_FRAME_INDEX_START_ACK, 0, trace_id,
                      ok, strlen(ok));
}

static int handle_index_status(runtime_state_t *state, int client_fd,
                                uint64_t trace_id) {
    if (!state) return -1;
    pthread_mutex_lock(&g_index_lock);
    char body[256];
    int n = snprintf(body, sizeof(body),
                     "{\"phase\":\"%s\",\"files\":%zu,\"truncated\":%s,"
                     "\"started_at\":%lld,\"completed_at\":%lld}",
                     g_index_phase == 0 ? "idle" :
                     g_index_phase == 1 ? "building" : "ready",
                     g_index_count,
                     g_index_truncated ? "true" : "false",
                     (long long)g_index_started_at,
                     (long long)g_index_completed_at);
    pthread_mutex_unlock(&g_index_lock);
    return send_frame(client_fd, FTX2_FRAME_INDEX_STATUS_ACK, 0,
                      trace_id, body, (uint64_t)n);
}

static int handle_search_index(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *body, uint64_t body_len) {
    if (!state) return -1;
    /* Parse `{"query":"...","size_min":N,"size_max":N,"limit":N}`. */
    char qbuf[256] = {0};
    long long size_min = 0;
    long long size_max = 0;
    int limit = 200;
    if (body && body_len > 0 && body_len < 1024) {
        char tmp[1024];
        memcpy(tmp, body, body_len);
        tmp[body_len] = '\0';
        const char *p = strstr(tmp, "\"query\"");
        if (p) {
            p = strchr(p, ':');
            if (p) p = strchr(p, '"');
            if (p) {
                p++;
                const char *e = strchr(p, '"');
                if (e && (size_t)(e - p) < sizeof(qbuf)) {
                    memcpy(qbuf, p, (size_t)(e - p));
                    qbuf[e - p] = '\0';
                }
            }
        }
        p = strstr(tmp, "\"size_min\"");
        if (p) {
            p = strchr(p, ':');
            if (p) size_min = atoll(p + 1);
        }
        p = strstr(tmp, "\"size_max\"");
        if (p) {
            p = strchr(p, ':');
            if (p) size_max = atoll(p + 1);
        }
        p = strstr(tmp, "\"limit\"");
        if (p) {
            p = strchr(p, ':');
            if (p) limit = atoi(p + 1);
        }
    }
    if (limit <= 0 || limit > 5000) limit = 200;
    if (!qbuf[0]) strcpy(qbuf, "*");

    char *resp = malloc(256 * 1024);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_SEARCH_INDEX_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 256 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"results\":[");
    int wrote_one = 0;
    int matched = 0;
    pthread_mutex_lock(&g_index_lock);
    for (size_t i = 0; i < g_index_count && matched < limit; i++) {
        const char *path = g_index_entries[i].path;
        if (!path) continue;
        const char *base = strrchr(path, '/');
        base = base ? base + 1 : path;
        if (!glob_match(qbuf, base, 0)) continue;
        long long sz = g_index_entries[i].size;
        if (size_min > 0 && sz < size_min) continue;
        if (size_max > 0 && sz > size_max) continue;
        char esc_path[2048];
        json_escape_into(path, esc_path, sizeof(esc_path));
        if (n >= cap - 2300) break;
        if (wrote_one) resp[n++] = ',';
        wrote_one = 1;
        n += snprintf(resp + n, cap - n,
                      "{\"path\":\"%s\",\"size\":%lld}",
                      esc_path, sz);
        matched++;
    }
    pthread_mutex_unlock(&g_index_lock);
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc = send_frame(client_fd, FTX2_FRAME_SEARCH_INDEX_ACK, 0,
                        trace_id, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* ── App lifecycle (suspend/resume/kill/list) ────────────────────────── */

/* sceApplicationGetProcs returns up to N opaque proc-info blobs;
 * each is at least 4 bytes (app_id at offset 0). The full struct is
 * larger and FW-version-dependent, but every revision keeps app_id
 * first. We allocate N × sizeof(uint32_t) × 16 (256-byte safe slot
 * per entry — generous bound for any FW). 64 entries × 256 = 16 KB. */
#define APP_PROCS_MAX_COUNT 64
#define APP_PROCS_SLOT_BYTES 256

static int handle_app_lifecycle(runtime_state_t *state, int client_fd,
                                 uint64_t trace_id, const char *body,
                                 uint64_t body_len) {
    if (!state) return -1;
    char action[16] = {0};
    unsigned int app_id = 0;
    if (body && body_len > 0 && body_len < 1024) {
        char tmp[1024];
        memcpy(tmp, body, body_len);
        tmp[body_len] = '\0';
        const char *p = strstr(tmp, "\"action\"");
        if (p) {
            p = strchr(p, ':');
            if (p) p = strchr(p, '"');
            if (p) {
                p++;
                const char *e = strchr(p, '"');
                if (e && (size_t)(e - p) < sizeof(action)) {
                    memcpy(action, p, (size_t)(e - p));
                    action[e - p] = '\0';
                }
            }
        }
        p = strstr(tmp, "\"app_id\"");
        if (p) {
            p = strchr(p, ':');
            if (p) app_id = (unsigned int)atoll(p + 1);
        }
    }
    if (action[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"bad_action\"}";
        return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (strcmp(action, "list") == 0) {
        /* Heap-allocate the procs scratch + response buffers. The
         * mgmt thread frame is tight (the dispatcher itself uses
         * ~3 KiB of locals across nested calls) and a 16 KiB +
         * ~32 KiB pair on stack pushed close to the guard page on
         * some firmware revisions. Heap allocation costs one
         * malloc/free per call which is dwarfed by the sceApp* RPC. */
        const size_t resp_cap = 16384;
        unsigned char *buf = calloc(APP_PROCS_MAX_COUNT,
                                    APP_PROCS_SLOT_BYTES);
        char *resp = malloc(resp_cap);
        if (!buf || !resp) {
            free(buf); free(resp);
            const char *err = "{\"ok\":false,\"err\":\"out_of_memory\"}";
            return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK,
                              0, trace_id, err, strlen(err));
        }
        int count = 0;
        int rc = -1;
        if (resolve_sce_syscore() == 0 && p_sceApplicationGetProcs) {
            rc = p_sceApplicationGetProcs(buf, APP_PROCS_MAX_COUNT, &count);
        }
        if (rc != 0 || count < 0) count = 0;
        if (count > APP_PROCS_MAX_COUNT) count = APP_PROCS_MAX_COUNT;
        int n = 0;
        n += snprintf(resp + n, resp_cap - n,
                      "{\"ok\":true,\"action\":\"list\",\"apps\":[");
        for (int i = 0; i < count; i++) {
            unsigned int aid;
            memcpy(&aid, buf + (size_t)i * APP_PROCS_SLOT_BYTES,
                   sizeof(aid));
            if (n >= (int)resp_cap - 60) break;
            if (i > 0) resp[n++] = ',';
            n += snprintf(resp + n, resp_cap - n,
                          "{\"app_id\":%u}", aid);
        }
        if (n < (int)resp_cap - 2) {
            resp[n++] = ']';
            resp[n++] = '}';
        }
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);
        int sret = send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK,
                              0, trace_id, resp, (uint64_t)n);
        free(buf);
        free(resp);
        return sret;
    }
    if (app_id == 0) {
        const char *err = "{\"ok\":false,\"err\":\"app_id_required\"}";
        return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (resolve_sce_syscore() != 0) {
        const char *err = "{\"ok\":false,\"err\":\"libSceSysCore_unavailable\"}";
        return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int rc = -1;
    if (strcmp(action, "suspend") == 0) {
        rc = p_sceApplicationSuspend ? p_sceApplicationSuspend(app_id) : -1;
    } else if (strcmp(action, "resume") == 0) {
        rc = p_sceApplicationResume ? p_sceApplicationResume(app_id) : -1;
    } else if (strcmp(action, "kill") == 0) {
        rc = p_sceApplicationKill ? p_sceApplicationKill(app_id) : -1;
    } else {
        const char *err = "{\"ok\":false,\"err\":\"unknown_action\"}";
        return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[160];
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"action\":\"%s\",\"app_id\":%u,\"code\":%d}",
                     rc == 0 ? "true" : "false", action, app_id, rc);
    return send_frame(client_fd, FTX2_FRAME_APP_LIFECYCLE_ACK, 0,
                      trace_id, resp, (uint64_t)n);
}

/* ── Kernel log read ─────────────────────────────────────────────────── */

/* Open /dev/klog once and read what's currently buffered. The kernel
 * log device returns 0 bytes when the buffer is empty (non-blocking
 * by default). We cap reads at 64 KB per call so the response stays
 * bounded. */
static int handle_klog_read(runtime_state_t *state, int client_fd,
                             uint64_t trace_id, const char *body,
                             uint64_t body_len) {
    if (!state) return -1;
    size_t max_bytes = 16 * 1024;
    if (body && body_len > 0 && body_len < 256) {
        char tmp[260];
        memcpy(tmp, body, body_len);
        tmp[body_len] = '\0';
        const char *p = strstr(tmp, "\"max_bytes\"");
        if (p) {
            p = strchr(p, ':');
            if (p) {
                long long v = atoll(p + 1);
                if (v > 0) {
                    max_bytes = (size_t)v;
                    if (max_bytes > 64 * 1024) max_bytes = 64 * 1024;
                }
            }
        }
    }
    int fd = open("/dev/klog", O_RDONLY | O_NONBLOCK);
    if (fd < 0) {
        const char *err = "open_klog_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, strlen(err));
    }
    char *buf = malloc(max_bytes);
    if (!buf) {
        close(fd);
        const char *err = "klog_oom";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, strlen(err));
    }
    ssize_t n = read(fd, buf, max_bytes);
    close(fd);
    if (n < 0) n = 0;
    int rc = send_frame(client_fd, FTX2_FRAME_KLOG_READ_ACK, 0,
                        trace_id, buf, (uint64_t)n);
    free(buf);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* ── Network interface listing ───────────────────────────────────────── */

static int handle_net_interfaces(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id) {
    if (!state) return -1;
    /* Two-path enumeration:
     *
     *   1) sceNetGetIfList — Sony's official API. Has rich fields
     *      (mtu, flags, bandwidth, etc.) but empirically fails on
     *      FW 9.60 retail (returns non-zero rc or zero count) even
     *      with elevated authid. Try it first; degrade silently on
     *      failure.
     *
     *   2) getifaddrs — FreeBSD libc walks PF_ROUTE directly. Works
     *      everywhere the OS has interfaces. Returns name + IPv4 +
     *      IPv6 + MAC + flags + (via SIOCGIFMTU) mtu.
     *
     * Both produce the same JSON shape:
     *   {"interfaces":[{name,mac,ipv4[,ipv6],mtu,flags,up},…],
     *    "source":"sceNetGetIfList"|"getifaddrs"}
     *
     * The desktop UI doesn't branch on source; it just renders the
     * entries. `source` is a diag hint so we know which path won. */
    /* Heap-allocate the two big buffers so this handler's stack
     * stays small (~1 KB). Pre-2.12.0 the on-stack version added
     * ~29 KB (resp[8192] + if_buf[16*0x500]=20480 + seen_names);
     * combined with deep getifaddrs internals and the mgmt-thread
     * accept-thread fallback path (runtime.c near the mgmt cap),
     * that risks underflow on small pthread stacks. Same pattern
     * handle_proc_modules uses at line ~10613. */
    const size_t resp_cap = 8192;
    char *resp = malloc(resp_cap);
    if (!resp) {
        const char *err = "{\"err\":\"oom\",\"interfaces\":[]}";
        return send_frame(client_fd, FTX2_FRAME_NET_INTERFACES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int n = 0;
    int wrote_any = 0;
    const char *source = NULL;
    n += snprintf(resp + n, resp_cap - n, "{\"interfaces\":[");

    /* ── Path 1: sceNetGetIfList ── */
    if (resolve_sce_net() == 0) {
        const size_t if_buf_sz = (size_t)NET_IF_MAX_ENTRIES * NET_IF_ENTRY_BYTES;
        unsigned char *if_buf = malloc(if_buf_sz);
        if (!if_buf) {
            free(resp);
            const char *err = "{\"err\":\"oom\",\"interfaces\":[]}";
            return send_frame(client_fd, FTX2_FRAME_NET_INTERFACES_ACK, 0,
                              trace_id, err, strlen(err));
        }
        memset(if_buf, 0, if_buf_sz);
        int count = 0;
        int sce_rc = p_sceNetGetIfList(if_buf, NET_IF_MAX_ENTRIES, &count);
        if (sce_rc == 0 && count > 0) {
            source = "sceNetGetIfList";
            if (count > NET_IF_MAX_ENTRIES) count = NET_IF_MAX_ENTRIES;
            /* Per psdevwiki SceNetIfInfo offsets (stable 9.x–12.x):
             *   +0    flags (u32)
             *   +8    name (32 bytes, null-terminated)
             *   +0x28 mtu (u32)
             *   +0x34 mac (6 bytes)
             *   +0x40 ipv4 addr (4 bytes) */
            for (int i = 0; i < count; i++) {
                const unsigned char *e = if_buf + (size_t)i * NET_IF_ENTRY_BYTES;
                char name_safe[33], name_esc[80];
                memcpy(name_safe, e + 8, 32);
                name_safe[32] = '\0';
                json_escape_into(name_safe, name_esc, sizeof(name_esc));
                const unsigned char *mac = e + 0x34;
                const unsigned char *ipv4 = e + 0x40;
                unsigned int mtu, flags;
                memcpy(&mtu,   e + 0x28, sizeof(mtu));
                memcpy(&flags, e,        sizeof(flags));
                if (n >= (int)resp_cap - 256) break;
                if (wrote_any) resp[n++] = ',';
                wrote_any = 1;
                n += snprintf(resp + n, resp_cap - n,
                              "{\"name\":\"%s\","
                              "\"mac\":\"%02x:%02x:%02x:%02x:%02x:%02x\","
                              "\"ipv4\":\"%u.%u.%u.%u\","
                              "\"mtu\":%u,\"flags\":%u}",
                              name_esc,
                              mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                              ipv4[0], ipv4[1], ipv4[2], ipv4[3],
                              mtu, flags);
            }
        }
        free(if_buf);
    }

    /* ── Path 2: getifaddrs fallback ──
     * Reached when Sony's API returned no entries (failure or empty).
     * Re-uses the same `resp` buffer + wrote_any counter so the JSON
     * stays well-formed regardless of which path filled it. */
    if (!wrote_any) {
        struct ifaddrs *ifa_head = NULL;
        if (getifaddrs(&ifa_head) == 0 && ifa_head) {
            source = "getifaddrs";
            char seen_names[NET_IF_MAX_ENTRIES][32];
            int seen_count = 0;
            for (struct ifaddrs *ifa = ifa_head; ifa; ifa = ifa->ifa_next) {
                if (!ifa->ifa_name) continue;
                /* Skip names we've already emitted (one entry per
                 * interface, with all addresses collated). */
                int dup = 0;
                for (int j = 0; j < seen_count; j++) {
                    if (strcmp(seen_names[j], ifa->ifa_name) == 0) { dup = 1; break; }
                }
                if (dup) continue;
                if (seen_count >= NET_IF_MAX_ENTRIES) break;
                snprintf(seen_names[seen_count], sizeof(seen_names[0]),
                         "%s", ifa->ifa_name);
                seen_count++;

                char ipv4[INET_ADDRSTRLEN]  = "";
                char ipv6[INET6_ADDRSTRLEN] = "";
                char mac[18]                = "";
                for (struct ifaddrs *q = ifa_head; q; q = q->ifa_next) {
                    if (!q->ifa_name || !q->ifa_addr) continue;
                    if (strcmp(q->ifa_name, ifa->ifa_name) != 0) continue;
                    if (q->ifa_addr->sa_family == AF_INET && !ipv4[0]) {
                        const struct sockaddr_in *sa =
                            (const struct sockaddr_in *)q->ifa_addr;
                        inet_ntop(AF_INET, &sa->sin_addr, ipv4, sizeof(ipv4));
                    } else if (q->ifa_addr->sa_family == AF_INET6 && !ipv6[0]) {
                        const struct sockaddr_in6 *sa =
                            (const struct sockaddr_in6 *)q->ifa_addr;
                        inet_ntop(AF_INET6, &sa->sin6_addr, ipv6, sizeof(ipv6));
                    } else if (q->ifa_addr->sa_family == AF_LINK && !mac[0]) {
                        const struct sockaddr_dl *sdl =
                            (const struct sockaddr_dl *)q->ifa_addr;
                        if (sdl->sdl_alen == 6) {
                            const unsigned char *m =
                                (const unsigned char *)LLADDR(sdl);
                            snprintf(mac, sizeof(mac),
                                     "%02x:%02x:%02x:%02x:%02x:%02x",
                                     m[0], m[1], m[2], m[3], m[4], m[5]);
                        }
                    }
                }
                unsigned int mtu = 0;
                int sk = socket(AF_INET, SOCK_DGRAM, 0);
                if (sk >= 0) {
                    struct ifreq ifr;
                    memset(&ifr, 0, sizeof(ifr));
                    strncpy(ifr.ifr_name, ifa->ifa_name, IFNAMSIZ - 1);
                    if (ioctl(sk, SIOCGIFMTU, &ifr) == 0) {
                        mtu = (unsigned int)ifr.ifr_mtu;
                    }
                    close(sk);
                }
                /* Skip purely-down placeholder interfaces with no
                 * address — those are tunnel slots Sony's UI hides. */
                if (!ipv4[0] && !ipv6[0] && !mac[0]) continue;
                if (n >= (int)resp_cap - 384) break;
                if (wrote_any) resp[n++] = ',';
                wrote_any = 1;
                char name_esc[80];
                json_escape_into(ifa->ifa_name, name_esc, sizeof(name_esc));
                int up = (ifa->ifa_flags & IFF_UP) ? 1 : 0;
                n += snprintf(resp + n, resp_cap - n,
                              "{\"name\":\"%s\",\"flags\":%u,\"mtu\":%u,"
                              "\"mac\":\"%s\",\"ipv4\":\"%s\",\"ipv6\":\"%s\","
                              "\"up\":%s}",
                              name_esc, (unsigned)ifa->ifa_flags,
                              mtu, mac, ipv4, ipv6,
                              up ? "true" : "false");
            }
            freeifaddrs(ifa_head);
        }
    }

    if (!wrote_any) {
        free(resp);
        const char *err =
            "{\"err\":\"no_interfaces_reported\",\"interfaces\":[],"
            "\"hint\":\"both sceNetGetIfList and getifaddrs returned no usable interfaces\"}";
        return send_frame(client_fd, FTX2_FRAME_NET_INTERFACES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    n += snprintf(resp + n, resp_cap - n,
                  "],\"source\":\"%s\"}", source ? source : "unknown");
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = send_frame(client_fd, FTX2_FRAME_NET_INTERFACES_ACK, 0,
                        trace_id, resp, (uint64_t)n);
    free(resp);
    return rc;
}

/* ── Peripheral control (BD/USB power) ───────────────────────────────── */

static int handle_peripheral_control(runtime_state_t *state, int client_fd,
                                      uint64_t trace_id, const char *body,
                                      uint64_t body_len) {
    if (!state) return -1;
    char action[32] = {0};
    int port = 0;
    if (body && body_len > 0 && body_len < 512) {
        char tmp[516];
        memcpy(tmp, body, body_len);
        tmp[body_len] = '\0';
        const char *p = strstr(tmp, "\"action\"");
        if (p) {
            p = strchr(p, ':');
            if (p) p = strchr(p, '"');
            if (p) {
                p++;
                const char *e = strchr(p, '"');
                if (e && (size_t)(e - p) < sizeof(action)) {
                    memcpy(action, p, (size_t)(e - p));
                    action[e - p] = '\0';
                }
            }
        }
        p = strstr(tmp, "\"port\"");
        if (p) {
            p = strchr(p, ':');
            if (p) port = atoi(p + 1);
        }
    }
    resolve_sce_kernel_extras();
    int rc = -1;
    if (strcmp(action, "bd_power_off") == 0) {
        rc = p_sceKernelIccControlBDPowerState
                ? p_sceKernelIccControlBDPowerState(0) : -1;
    } else if (strcmp(action, "bd_power_on") == 0) {
        rc = p_sceKernelIccControlBDPowerState
                ? p_sceKernelIccControlBDPowerState(1) : -1;
    } else if (strcmp(action, "eject_disc") == 0) {
        /* Disc eject is "BD power off then on" on PS5 — there's no
         * dedicated eject syscall. State 2 corresponds to "eject"
         * per psdevwiki notes, but we fall back to power-cycle if
         * it's rejected. */
        if (p_sceKernelIccControlBDPowerState) {
            rc = p_sceKernelIccControlBDPowerState(2);
            if (rc != 0) {
                p_sceKernelIccControlBDPowerState(0);
                rc = p_sceKernelIccControlBDPowerState(1);
            }
        }
    } else if (strcmp(action, "usb_port_off") == 0) {
        rc = p_sceKernelIccControlUSBPowerState
                ? p_sceKernelIccControlUSBPowerState(port, 0) : -1;
    } else if (strcmp(action, "usb_port_on") == 0) {
        rc = p_sceKernelIccControlUSBPowerState
                ? p_sceKernelIccControlUSBPowerState(port, 1) : -1;
    } else {
        const char *err = "{\"ok\":false,\"err\":\"unknown_action\"}";
        return send_frame(client_fd, FTX2_FRAME_PERIPHERAL_CONTROL_ACK,
                          0, trace_id, err, strlen(err));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[160];
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"action\":\"%s\",\"port\":%d,\"code\":%d}",
                     rc == 0 ? "true" : "false", action, port, rc);
    return send_frame(client_fd, FTX2_FRAME_PERIPHERAL_CONTROL_ACK,
                      0, trace_id, resp, (uint64_t)n);
}

/* ── Shell command exec ──────────────────────────────────────────────── */

/* Run a single shell command via popen(), capturing stdout+stderr.
 *
 * Security note: the `cmd` string is passed verbatim to /bin/sh -c.
 * That's intentional — the renderer uses this for explicit
 * "advanced debugging" workflows (the user typed a command into a
 * shell prompt). We do NOT interpolate any other field into the
 * shell string (no separate cwd/env), so there is no injection
 * surface beyond what the user themselves typed. The whole RPC
 * surface (FS_DELETE, FS_CHMOD, etc.) already trusts an
 * authenticated LAN caller; this is the same trust boundary.
 *
 * We cap stdout+stderr at 256 KB and timeout at 30s. */
/* ── Built-in shell command interpreter ─────────────────────────────
 *
 * PS5 doesn't ship a shell binary (no /bin/sh, no /system/bin/sh,
 * nothing). popen("...", "r") in handle_shell_exec always fails on
 * PS5 because there's nothing for the libc shell-fork to exec. We
 * implement a tiny built-in interpreter that handles the commands
 * a PS5 operator actually wants — directory listing, file read,
 * uname, ps, mount, sysctl, df, id — using the same syscalls the
 * rest of the payload already uses.
 *
 * Returns 0 on recognised command (sets *out_text + *out_exit;
 * caller frees out_text). Returns -1 on unrecognised command (caller
 * may fall through to popen path or surface "not supported").
 *
 * Threading: stateless — each invocation creates its own buffer. No
 * shared state, no locking. */
static char *strdup_safe(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s);
    char *r = malloc(n + 1);
    if (r) memcpy(r, s, n + 1);
    return r;
}

/* Split cmd into argv on whitespace, with POSIX-ish quoting:
 *   - `'literal'`     — preserves everything verbatim incl. backslash
 *   - `"weak quotes"` — preserves everything but allows \" and \\ escape
 *   - `\X` outside quotes — keeps X literal (eats one char of whitespace)
 *
 * Writes NULs in-place into `cmd`. argv MUST be sized for at least
 * `max_args` slots. Returns argc.
 *
 * Rewritten in 2.13.0 — the pre-rewrite version was whitespace-only,
 * which broke `cat "/path with spaces"` (cat would see two args
 * `"/path` and `with` and ENOENT immediately). All built-ins that
 * take a path argument now handle quoted paths correctly. */
static int shell_split(char *cmd, char *argv[], int max_args) {
    int argc = 0;
    char *p = cmd;
    char *w = cmd; /* write cursor — handles in-place quote-stripping */
    while (*p && argc < max_args) {
        /* Skip leading whitespace. */
        while (*p == ' ' || *p == '\t') p++;
        if (!*p) break;
        /* Each arg starts at the current write cursor — quote-stripping
         * may shift later chars back, so the visible arg pointer is `w`
         * not `p` at this moment. */
        argv[argc++] = w;
        while (*p) {
            char c = *p;
            if (c == ' ' || c == '\t') break;
            if (c == '\'') {
                /* Single quote: copy everything to next ' verbatim. */
                p++;
                while (*p && *p != '\'') *w++ = *p++;
                if (*p == '\'') p++;
                continue;
            }
            if (c == '"') {
                /* Double quote: copy with \" and \\ escapes recognised. */
                p++;
                while (*p && *p != '"') {
                    if (*p == '\\' && (p[1] == '"' || p[1] == '\\')) {
                        *w++ = p[1];
                        p += 2;
                    } else {
                        *w++ = *p++;
                    }
                }
                if (*p == '"') p++;
                continue;
            }
            if (c == '\\' && p[1]) {
                /* Bare backslash escape: take next char literal. */
                *w++ = p[1];
                p += 2;
                continue;
            }
            *w++ = *p++;
        }
        while (*p == ' ' || *p == '\t') p++;
        *w++ = '\0'; /* terminate this arg */
    }
    return argc;
}

/* Append `fmt`-formatted output to a dynamically grown buffer. Returns
 * the new len; updates *cap and *buf on grow. */
static size_t shell_appendf(char **buf, size_t *cap, size_t len,
                             const char *fmt, ...) {
    const size_t max_cap = 256u * 1024u;
    va_list ap;
    while (1) {
        if (len >= max_cap - 1) return len;
        if (!*buf || *cap == 0) {
            char *nb = malloc(1024u);
            if (!nb) return len;
            *buf = nb;
            *cap = 1024u;
            (*buf)[0] = '\0';
        }
        va_start(ap, fmt);
        size_t avail = (*cap > len) ? (*cap - len) : 0;
        int n = vsnprintf(*buf + len, avail, fmt, ap);
        va_end(ap);
        if (n < 0) return len;
        if ((size_t)n < avail) return len + (size_t)n;
        size_t want = (*cap == 0 ? 1024u : *cap * 2u);
        if (want < len + (size_t)n + 1) want = len + (size_t)n + 1;
        if (want > max_cap) want = max_cap;
        if (*cap >= max_cap) return len;
        char *nb = realloc(*buf, want);
        if (!nb) return len;
        *buf = nb;
        *cap = want;
    }
}

static pthread_mutex_t g_shell_cwd_mtx = PTHREAD_MUTEX_INITIALIZER;

#define PS5UPLOAD2_SHELL_SESSIONS 16
typedef struct {
    int in_use;
    char session_id[96];
    char cwd[1024];
    uint64_t last_used;
} shell_session_t;
static pthread_mutex_t g_shell_session_mtx = PTHREAD_MUTEX_INITIALIZER;
static shell_session_t g_shell_sessions[PS5UPLOAD2_SHELL_SESSIONS];
static uint64_t g_shell_session_seq = 0;

static const char *shell_valid_cwd(const char *cwd) {
    return (cwd && cwd[0] == '/') ? cwd : "/";
}

static int shell_valid_session_id(const char *session_id) {
    if (!session_id || !session_id[0]) return 0;
    for (const char *p = session_id; *p; p++) {
        unsigned char c = (unsigned char)*p;
        if (c < 0x21 || c > 0x7e || c == '"' || c == '\\') return 0;
    }
    return 1;
}

static void shell_session_get(const char *session_id, const char *fallback,
                              char *out, size_t cap) {
    const char *base = shell_valid_cwd(fallback);
    if (!out || cap == 0) return;
    snprintf(out, cap, "%s", base);
    if (!shell_valid_session_id(session_id)) return;
    pthread_mutex_lock(&g_shell_session_mtx);
    g_shell_session_seq += 1;
    int free_idx = -1;
    int oldest_idx = 0;
    uint64_t oldest = UINT64_MAX;
    for (int i = 0; i < PS5UPLOAD2_SHELL_SESSIONS; i++) {
        shell_session_t *s = &g_shell_sessions[i];
        if (s->in_use && strcmp(s->session_id, session_id) == 0) {
            s->last_used = g_shell_session_seq;
            snprintf(out, cap, "%s", shell_valid_cwd(s->cwd));
            pthread_mutex_unlock(&g_shell_session_mtx);
            return;
        }
        if (!s->in_use && free_idx < 0) free_idx = i;
        if (s->in_use && s->last_used < oldest) {
            oldest = s->last_used;
            oldest_idx = i;
        }
    }
    int idx = free_idx >= 0 ? free_idx : oldest_idx;
    shell_session_t *s = &g_shell_sessions[idx];
    memset(s, 0, sizeof(*s));
    s->in_use = 1;
    s->last_used = g_shell_session_seq;
    snprintf(s->session_id, sizeof(s->session_id), "%s", session_id);
    snprintf(s->cwd, sizeof(s->cwd), "%s", base);
    snprintf(out, cap, "%s", s->cwd);
    pthread_mutex_unlock(&g_shell_session_mtx);
}

static void shell_session_set(const char *session_id, const char *cwd) {
    if (!shell_valid_session_id(session_id)) return;
    const char *next = shell_valid_cwd(cwd);
    pthread_mutex_lock(&g_shell_session_mtx);
    g_shell_session_seq += 1;
    int free_idx = -1;
    int oldest_idx = 0;
    uint64_t oldest = UINT64_MAX;
    for (int i = 0; i < PS5UPLOAD2_SHELL_SESSIONS; i++) {
        shell_session_t *s = &g_shell_sessions[i];
        if (s->in_use && strcmp(s->session_id, session_id) == 0) {
            snprintf(s->cwd, sizeof(s->cwd), "%s", next);
            s->last_used = g_shell_session_seq;
            pthread_mutex_unlock(&g_shell_session_mtx);
            return;
        }
        if (!s->in_use && free_idx < 0) free_idx = i;
        if (s->in_use && s->last_used < oldest) {
            oldest = s->last_used;
            oldest_idx = i;
        }
    }
    int idx = free_idx >= 0 ? free_idx : oldest_idx;
    shell_session_t *s = &g_shell_sessions[idx];
    memset(s, 0, sizeof(*s));
    s->in_use = 1;
    s->last_used = g_shell_session_seq;
    snprintf(s->session_id, sizeof(s->session_id), "%s", session_id);
    snprintf(s->cwd, sizeof(s->cwd), "%s", next);
    pthread_mutex_unlock(&g_shell_session_mtx);
}

static int shell_json_string_field(const char *body, uint64_t body_len,
                                   const char *field, char *out, size_t cap) {
    if (!body || !field || !out || cap == 0) return -1;
    out[0] = '\0';
    char key[80];
    int kn = snprintf(key, sizeof(key), "\"%s\"", field);
    if (kn < 0 || (size_t)kn >= sizeof(key)) return -1;
    const char *end = body + body_len;
    const char *p = body;
    while (p < end) {
        const char *hit = strstr(p, key);
        if (!hit || hit >= end) return -1;
        p = hit + (size_t)kn;
        while (p < end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
        if (p >= end || *p != ':') continue;
        p++;
        while (p < end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
        if (p >= end || *p != '"') return -1;
        p++;
        size_t n = 0;
        while (p < end && *p != '"') {
            unsigned char c = (unsigned char)*p++;
            if (c == '\\' && p < end) {
                unsigned char e = (unsigned char)*p++;
                if (e == '"' || e == '\\' || e == '/') c = e;
                else if (e == 'n') c = '\n';
                else if (e == 'r') c = '\r';
                else if (e == 't') c = '\t';
                else if (e == 'b') c = '\b';
                else if (e == 'f') c = '\f';
                else if (e == 'u') {
                    if (p + 4 <= end) p += 4;
                    c = '?';
                } else {
                    c = e;
                }
            }
            if (n + 1 < cap) out[n++] = (char)c;
        }
        if (p >= end || *p != '"') return -1;
        out[n] = '\0';
        return 0;
    }
    return -1;
}

static int shell_send_json_result(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, int exit_code,
                                  const char *stdout_text,
                                  const char *cwd_text,
                                  const char *session_id,
                                  int timed_out) {
    size_t out_len = stdout_text ? strlen(stdout_text) : 0;
    size_t cwd_len = cwd_text ? strlen(cwd_text) : 1;
    size_t sid_len = session_id ? strlen(session_id) : 0;
    char *resp = malloc(out_len + cwd_len + sid_len + 680);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_SHELL_EXEC_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int n = 0;
    size_t cap = out_len + cwd_len + sid_len + 680;
    n += snprintf(resp + n, cap - (size_t)n,
                  "{\"exit_code\":%d,\"timed_out\":%s,\"stdout\":\"",
                  exit_code, timed_out ? "true" : "false");
    for (size_t i = 0; i < out_len && (size_t)n < cap - 8; i++) {
        unsigned char c = (unsigned char)stdout_text[i];
        if (c == '\\' || c == '"') { resp[n++] = '\\'; resp[n++] = (char)c; }
        else if (c == '\n') { resp[n++] = '\\'; resp[n++] = 'n'; }
        else if (c == '\r') { resp[n++] = '\\'; resp[n++] = 'r'; }
        else if (c == '\t') { resp[n++] = '\\'; resp[n++] = 't'; }
        else if (c < 0x20) { n += snprintf(resp + n, cap - (size_t)n, "\\u%04x", c); }
        else { resp[n++] = (char)c; }
    }
    n += snprintf(resp + n, cap - (size_t)n, "\",\"cwd\":\"");
    const char *cwd_src = (cwd_text && cwd_text[0]) ? cwd_text : "/";
    for (size_t i = 0; cwd_src[i] && (size_t)n < cap - 8; i++) {
        unsigned char c = (unsigned char)cwd_src[i];
        if (c == '\\' || c == '"') { resp[n++] = '\\'; resp[n++] = (char)c; }
        else if (c == '\n') { resp[n++] = '\\'; resp[n++] = 'n'; }
        else if (c == '\r') { resp[n++] = '\\'; resp[n++] = 'r'; }
        else if (c == '\t') { resp[n++] = '\\'; resp[n++] = 't'; }
        else if (c < 0x20) { n += snprintf(resp + n, cap - (size_t)n, "\\u%04x", c); }
        else { resp[n++] = (char)c; }
    }
    n += snprintf(resp + n, cap - (size_t)n, "\",\"session_id\":\"");
    const char *sid_src = session_id ? session_id : "";
    for (size_t i = 0; sid_src[i] && (size_t)n < cap - 8; i++) {
        unsigned char c = (unsigned char)sid_src[i];
        if (c == '\\' || c == '"') { resp[n++] = '\\'; resp[n++] = (char)c; }
        else if (c == '\n') { resp[n++] = '\\'; resp[n++] = 'n'; }
        else if (c == '\r') { resp[n++] = '\\'; resp[n++] = 'r'; }
        else if (c == '\t') { resp[n++] = '\\'; resp[n++] = 't'; }
        else if (c < 0x20) { n += snprintf(resp + n, cap - (size_t)n, "\\u%04x", c); }
        else { resp[n++] = (char)c; }
    }
    n += snprintf(resp + n, cap - (size_t)n, "\"}");
    int rc = send_frame(client_fd, FTX2_FRAME_SHELL_EXEC_ACK, 0,
                        trace_id, resp, (uint64_t)n);
    free(resp);
    if (state) {
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);
    }
    return rc;
}

static int shell_join_path(const char *cwd, const char *path,
                           char *out, size_t cap) {
    if (!path || !*path) path = "/";
    if (path[0] == '/') {
        int n = snprintf(out, cap, "%s", path);
        return (n >= 0 && (size_t)n < cap) ? 0 : -1;
    }
    if (!cwd || cwd[0] != '/') cwd = "/";
    int n = snprintf(out, cap, "%s%s%s",
                     cwd,
                     strcmp(cwd, "/") == 0 ? "" : "/",
                     path);
    return (n >= 0 && (size_t)n < cap) ? 0 : -1;
}

static int shell_resolve_dir(const char *cwd, const char *path,
                             char *out, size_t cap, char *err, size_t err_cap) {
    char joined[1024];
    if (shell_join_path(cwd, path, joined, sizeof(joined)) != 0) {
        snprintf(err, err_cap, "path too long");
        return -1;
    }
    char resolved[1024];
    if (!realpath(joined, resolved)) {
        snprintf(err, err_cap, "%s", strerror(errno));
        return -1;
    }
    struct stat st;
    if (stat(resolved, &st) != 0) {
        snprintf(err, err_cap, "%s", strerror(errno));
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        snprintf(err, err_cap, "not a directory");
        return -1;
    }
    int n = snprintf(out, cap, "%s", resolved);
    if (n < 0 || (size_t)n >= cap) {
        snprintf(err, err_cap, "path too long");
        return -1;
    }
    return 0;
}

static int shell_ls_path(const char *path, char **out_text, int *out_exit) {
    if (!path || !out_text || !out_exit) return -1;
    *out_text = NULL;
    *out_exit = 0;
    char *out = NULL;
    size_t cap = 0, len = 0;
    DIR *dp = opendir(path);
    if (!dp) {
        len = shell_appendf(&out, &cap, len, "ls: %s: %s\n",
                            path, strerror(errno));
        *out_text = out;
        *out_exit = 1;
        return 0;
    }
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.' && e->d_name[1] == '\0') continue;
        if (e->d_name[0] == '.' && e->d_name[1] == '.' && e->d_name[2] == '\0') continue;
        char child[1024];
        struct stat st;
        char type_c = '?';
        long long size = 0;
        int cn = snprintf(child, sizeof(child), "%s/%s",
                          strcmp(path, "/") == 0 ? "" : path,
                          e->d_name);
        if (cn >= 0 && (size_t)cn < sizeof(child) && stat(child, &st) == 0) {
            if (S_ISDIR(st.st_mode)) type_c = 'd';
            else if (S_ISREG(st.st_mode)) type_c = 'f';
            else if (S_ISLNK(st.st_mode)) type_c = 'l';
            else type_c = 'o';
            size = (long long)st.st_size;
        }
        len = shell_appendf(&out, &cap, len, "%c %12lld  %s\n",
                            type_c, size, e->d_name);
    }
    closedir(dp);
    if (!out) out = strdup_safe("");
    *out_text = out;
    return 0;
}

static int handle_shell_builtin(const char *cmd_in, char **out_text,
                                 int *out_exit) {
    if (!cmd_in || !out_text || !out_exit) return -1;
    *out_text = NULL;
    *out_exit = 0;
    char cmdbuf[2100];
    snprintf(cmdbuf, sizeof(cmdbuf), "%s", cmd_in);
    char *argv[32];
    int argc = shell_split(cmdbuf, argv, 32);
    if (argc == 0) return -1;
    const char *prog = argv[0];

    char *out = NULL;
    size_t cap = 0, len = 0;

    if (strcmp(prog, "help") == 0) {
        len = shell_appendf(&out, &cap, len,
            "ps5upload built-in shell commands (PS5 has no /bin/sh).\n"
            "Quoting: 'literal', \"weak\", \\X — paths with spaces OK.\n"
            "\n"
            "Inspect:\n"
            "  help                    show this list\n"
            "  ls [path]               list directory (default /)\n"
            "  cat <path>              print file contents (8 KiB cap)\n"
            "  head [-n N] <path>      first N lines (default 10)\n"
            "  tail [-n N] <path>      last N lines (default 10)\n"
            "  wc [-lwc] <path>        line / word / byte counts\n"
            "  stat <path>             show file metadata\n"
            "  file <path>...          detect file type by magic bytes\n"
            "  xxd | hexdump [-C] <p>  canonical hex+ASCII (16 KiB cap)\n"
            "  find [path] [-name G] [-type f|d|l]   FTS walker\n"
            "  grep [-riElc] PAT path  POSIX regex search\n"
            "  du [-sh] <path>...      disk usage\n"
            "  sfoinfo <path>          parse param.sfo key/value pairs\n"
            "\n"
            "Filesystem:\n"
            "  cd [path]               change working dir (default /)\n"
            "  pwd                     print working dir\n"
            "  touch <path>...         create or bump mtime\n"
            "  mkdir [-p] <path>...    create directory\n"
            "  rmdir <path>...         remove empty directory\n"
            "  rm [-rf] <path>...      delete (refuses /system, /preinst)\n"
            "  cp [-r] SRC... DST      file or dir copy (256 MiB cap)\n"
            "  mv SRC... DST           rename, cross-FS copy+unlink\n"
            "  chmod [-R] OCT <path>   change mode (octal only)\n"
            "  ln -s TARGET LINK       create symbolic link\n"
            "  which <name>            find homebrew binary by name\n"
            "  mount                   active mount table\n"
            "  mtrw [/path]            remount /system rw (needs kstuff)\n"
            "  df                      filesystem usage\n"
            "\n"
            "Processes:\n"
            "  ps                      running processes (pid + name)\n"
            "  pid <name>              find pid(s) by substring match\n"
            "  kill [-N] <pid>...      send signal N (default 15/TERM)\n"
            "\n"
            "System:\n"
            "  date [+FMT]             current UTC time (strftime format)\n"
            "  uname [-a]              kernel info\n"
            "  hostname                kern.hostname sysctl\n"
            "  id                      effective uid/gid/authid\n"
            "  env                     environment variables\n"
            "  sysctl <name>           read a sysctl by name\n"
            "  sleep <secs>            sleep N seconds (1-30)\n"
            "  sync                    flush dirty buffers\n"
            "  klog [-n N]             last N bytes of /dev/klog\n"
            "  notify <msg...>         PS5 toast notification\n"
            "\n"
            "Path utils:\n"
            "  basename <path>         strip dir part\n"
            "  dirname <path>          strip file part\n"
            "\n"
            "Misc:\n"
            "  true | false            exit code 0 / 1\n"
            "  echo <args...>          print args verbatim\n");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "true") == 0) { *out_text = strdup_safe(""); return 0; }
    if (strcmp(prog, "false") == 0) {
        *out_text = strdup_safe("");
        *out_exit = 1;
        return 0;
    }
    if (strcmp(prog, "cd") == 0) {
        const char *path = argc >= 2 ? argv[1] : "/";
        if (chdir(path) != 0) {
            len = shell_appendf(&out, &cap, len, "cd: %s: %s\n",
                                 path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char cwd[1024];
        if (getcwd(cwd, sizeof(cwd))) {
            len = shell_appendf(&out, &cap, len, "%s\n", cwd);
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "pwd") == 0) {
        char cwd[1024];
        if (getcwd(cwd, sizeof(cwd))) {
            len = shell_appendf(&out, &cap, len, "%s\n", cwd);
        } else {
            len = shell_appendf(&out, &cap, len, "/\n");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "echo") == 0) {
        for (int i = 1; i < argc; i++) {
            len = shell_appendf(&out, &cap, len, "%s%s",
                                 argv[i], i + 1 < argc ? " " : "\n");
        }
        if (argc == 1) len = shell_appendf(&out, &cap, len, "\n");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "uname") == 0) {
        struct utsname u;
        if (uname(&u) != 0) {
            *out_text = strdup_safe("uname: failed\n");
            *out_exit = 1;
            return 0;
        }
        int all = (argc >= 2 && strcmp(argv[1], "-a") == 0);
        if (all) {
            len = shell_appendf(&out, &cap, len, "%s %s %s %s %s\n",
                                 u.sysname, u.nodename, u.release,
                                 u.version, u.machine);
        } else {
            len = shell_appendf(&out, &cap, len, "%s\n", u.sysname);
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "hostname") == 0) {
        char host[256] = {0};
        size_t hl = sizeof(host);
        int mib[2] = {CTL_KERN, KERN_HOSTNAME};
        if (sysctl(mib, 2, host, &hl, NULL, 0) == 0) {
            len = shell_appendf(&out, &cap, len, "%s\n", host);
        } else {
            len = shell_appendf(&out, &cap, len, "(unknown)\n");
            *out_exit = 1;
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "id") == 0) {
        uid_t uid = getuid(), euid = geteuid();
        gid_t gid = getgid(), egid = getegid();
        pid_t pid = getpid();
        len = shell_appendf(&out, &cap, len,
                            "uid=%u euid=%u gid=%u egid=%u pid=%d "
                            "ucred_elevation_rc=%d\n",
                            (unsigned)uid, (unsigned)euid,
                            (unsigned)gid, (unsigned)egid, (int)pid,
                            g_ucred_elevation_rc);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "env") == 0) {
        extern char **environ;
        for (char **e = environ; e && *e; e++) {
            len = shell_appendf(&out, &cap, len, "%s\n", *e);
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sleep") == 0) {
        int s = argc >= 2 ? atoi(argv[1]) : 0;
        if (s < 1) s = 1;
        if (s > 30) s = 30;
        sleep((unsigned)s);
        *out_text = strdup_safe("");
        return 0;
    }
    if (strcmp(prog, "ls") == 0) {
        const char *path = argc >= 2 ? argv[1] : "/";
        return shell_ls_path(path, out_text, out_exit);
    }
    if (strcmp(prog, "cat") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("cat: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        const char *path = argv[1];
        int fd = open(path, O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len, "cat: %s: %s\n",
                                 path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char chunk[8192];
        ssize_t total = 0;
        ssize_t r;
        while ((r = read(fd, chunk, sizeof(chunk))) > 0) {
            len = shell_appendf(&out, &cap, len, "%.*s", (int)r, chunk);
            total += r;
            if (total > 8 * 1024) break;
        }
        close(fd);
        if (total > 8 * 1024) {
            len = shell_appendf(&out, &cap, len,
                                 "\n(... cat output capped at 8 KiB)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "stat") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("stat: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        struct stat st;
        if (stat(argv[1], &st) != 0) {
            len = shell_appendf(&out, &cap, len, "stat: %s: %s\n",
                                 argv[1], strerror(errno));
            *out_exit = 1;
        } else {
            len = shell_appendf(&out, &cap, len,
                                 "path: %s\nsize: %lld\nmode: 0%o\n"
                                 "uid: %u\ngid: %u\nmtime: %lld\n"
                                 "type: %s\n",
                                 argv[1], (long long)st.st_size,
                                 (unsigned)st.st_mode,
                                 (unsigned)st.st_uid, (unsigned)st.st_gid,
                                 (long long)st.st_mtime,
                                 S_ISDIR(st.st_mode) ? "dir" :
                                 S_ISREG(st.st_mode) ? "file" :
                                 S_ISLNK(st.st_mode) ? "link" : "other");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mount") == 0) {
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        for (int i = 0; i < n && mnts; i++) {
            len = shell_appendf(&out, &cap, len, "%-10s %-30s %s\n",
                                 mnts[i].f_fstypename,
                                 mnts[i].f_mntfromname,
                                 mnts[i].f_mntonname);
        }
        free(mnts);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "df") == 0) {
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        len = shell_appendf(&out, &cap, len,
                             "%-30s %12s %12s %12s  use%%\n",
                             "filesystem", "blocks", "used", "avail");
        for (int i = 0; i < n && mnts; i++) {
            uint64_t bs = mnts[i].f_bsize;
            uint64_t total = (uint64_t)mnts[i].f_blocks * bs;
            uint64_t free_b = (uint64_t)mnts[i].f_bfree * bs;
            uint64_t used = total - free_b;
            int pct = total > 0 ? (int)((used * 100) / total) : 0;
            len = shell_appendf(&out, &cap, len,
                                 "%-30s %12llu %12llu %12llu  %3d%%\n",
                                 mnts[i].f_mntonname,
                                 (unsigned long long)(total / 1024),
                                 (unsigned long long)(used  / 1024),
                                 (unsigned long long)(free_b / 1024),
                                 pct);
        }
        free(mnts);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "ps") == 0) {
        /* proc_list_get_json returns a JSON blob like
         *   {"ok":true,"procs":[{"pid":123,"name":"SceShellUI"},...]}
         * Re-parse the pid+name pairs into a pretty 2-column listing.
         * Avoids reaching for a JSON library — the format is fixed and
         * we own both producer + consumer. */
        char *jbuf = malloc(64 * 1024);
        if (!jbuf) {
            *out_text = strdup_safe("ps: oom\n");
            *out_exit = 1;
            return 0;
        }
        size_t jwritten = 0;
        const char *jerr = NULL;
        if (proc_list_get_json(jbuf, 64 * 1024, &jwritten, &jerr) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "ps: %s\n", jerr ? jerr : "failed");
            free(jbuf);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        /* Walk the JSON looking for "pid":N pairs and "name":"..." pairs.
         * Robust enough for our fixed shape; not a general JSON parser. */
        char *p = jbuf;
        while (p && *p) {
            char *pp = strstr(p, "\"pid\":");
            if (!pp) break;
            int pid_val = atoi(pp + 6);
            char *np = strstr(pp, "\"name\":\"");
            if (!np) break;
            np += 8;
            char *ne = strchr(np, '"');
            if (!ne) break;
            char name_buf[64];
            size_t nl = (size_t)(ne - np);
            if (nl >= sizeof(name_buf)) nl = sizeof(name_buf) - 1;
            memcpy(name_buf, np, nl);
            name_buf[nl] = '\0';
            len = shell_appendf(&out, &cap, len, "%6d  %s\n",
                                 pid_val, name_buf);
            p = ne + 1;
        }
        free(jbuf);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sysctl") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("sysctl: missing name\n");
            *out_exit = 1;
            return 0;
        }
        char val[1024] = {0};
        size_t vl = sizeof(val);
        if (sysctlbyname(argv[1], val, &vl, NULL, 0) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "sysctl: %s: %s\n", argv[1], strerror(errno));
            *out_exit = 1;
        } else {
            len = shell_appendf(&out, &cap, len, "%s\n", val);
        }
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 1 additions ───────────────────────────────────── */
    if (strcmp(prog, "date") == 0) {
        /* `date` (no arg) → default RFC-like format. `date +FMT` → user
         * format. UTC only — PS5 system clock is stored UTC, and the
         * Hardware tab has the proper TZ display if the user wants
         * local. */
        time_t t = time(NULL);
        struct tm tm_utc;
        gmtime_r(&t, &tm_utc);
        const char *fmt = "%Y-%m-%d %H:%M:%S UTC";
        char user_fmt[128];
        if (argc >= 2 && argv[1][0] == '+') {
            snprintf(user_fmt, sizeof(user_fmt), "%s", argv[1] + 1);
            fmt = user_fmt;
        }
        char buf[256];
        if (strftime(buf, sizeof(buf), fmt, &tm_utc) == 0) {
            *out_text = strdup_safe("date: bad format or output too long\n");
            *out_exit = 1;
            return 0;
        }
        len = shell_appendf(&out, &cap, len, "%s\n", buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "basename") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("basename: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        const char *p = strrchr(argv[1], '/');
        const char *base = p ? p + 1 : argv[1];
        if (*base == '\0') base = "/"; /* "/" → "/" */
        len = shell_appendf(&out, &cap, len, "%s\n", base);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "dirname") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("dirname: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        char tmp[1024];
        snprintf(tmp, sizeof(tmp), "%s", argv[1]);
        /* Strip trailing slashes except for root. */
        size_t L = strlen(tmp);
        while (L > 1 && tmp[L - 1] == '/') tmp[--L] = '\0';
        char *p = strrchr(tmp, '/');
        const char *d = ".";
        if (p == tmp) d = "/";
        else if (p) { *p = '\0'; d = tmp; }
        len = shell_appendf(&out, &cap, len, "%s\n", d);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "touch") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("touch: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            /* Create if missing, then bump mtime+atime to now. */
            int fd = open(argv[i], O_CREAT | O_WRONLY, 0644);
            if (fd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "touch: %s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            close(fd);
            /* Use utimes(NULL) = bump both to current wall clock. */
            if (utimes(argv[i], NULL) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "touch: %s: utimes: %s\n", argv[i],
                                     strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mkdir") == 0) {
        int parents = 0;
        int first_path = 1;
        if (argc >= 2 && strcmp(argv[1], "-p") == 0) {
            parents = 1;
            first_path = 2;
        }
        if (argc <= first_path) {
            *out_text = strdup_safe("mkdir: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first_path; i < argc; i++) {
            if (!parents) {
                if (mkdir(argv[i], 0755) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "mkdir: %s: %s\n", argv[i],
                                         strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* -p: walk components, mkdir each ignoring EEXIST. */
            char tmp[1024];
            int tn = snprintf(tmp, sizeof(tmp), "%s", argv[i]);
            if (tn < 0 || (size_t)tn >= sizeof(tmp)) {
                len = shell_appendf(&out, &cap, len,
                                     "mkdir: %s: path too long\n", argv[i]);
                any_err = 1;
                continue;
            }
            for (char *p = tmp + (tmp[0] == '/' ? 1 : 0); *p; p++) {
                if (*p == '/') {
                    *p = '\0';
                    if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
                        len = shell_appendf(&out, &cap, len,
                                             "mkdir: %s: %s\n", tmp,
                                             strerror(errno));
                        any_err = 1;
                        break;
                    }
                    *p = '/';
                }
            }
            if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
                len = shell_appendf(&out, &cap, len,
                                     "mkdir: %s: %s\n", tmp, strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "rmdir") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("rmdir: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            if (rmdir(argv[i]) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "rmdir: %s: %s\n", argv[i],
                                     strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "kill") == 0) {
        int sig = SIGTERM;
        int first = 1;
        /* Accept `kill -9 PID` or `kill -SIGKILL PID` (numeric only for
         * now — symbolic names would need a table). */
        if (argc >= 3 && argv[1][0] == '-') {
            int n = atoi(argv[1] + 1);
            if (n > 0 && n < 64) sig = n;
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("kill: missing PID\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first; i < argc; i++) {
            int pid = atoi(argv[i]);
            if (pid <= 0) {
                len = shell_appendf(&out, &cap, len,
                                     "kill: %s: not a pid\n", argv[i]);
                any_err = 1;
                continue;
            }
            if (kill((pid_t)pid, sig) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "kill: %d: %s\n", pid, strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sync") == 0) {
        sync();
        *out_text = strdup_safe("");
        return 0;
    }
    if (strcmp(prog, "notify") == 0) {
        /* Join argv[1..] with spaces and fire as a PS5 toast. Useful
         * for "I'm done, see this notification" from scripts. */
        if (argc < 2) {
            *out_text = strdup_safe("notify: missing message\n");
            *out_exit = 1;
            return 0;
        }
        char msg[1024] = {0};
        size_t mi = 0;
        for (int i = 1; i < argc && mi + 1 < sizeof(msg); i++) {
            int n = snprintf(msg + mi, sizeof(msg) - mi, "%s%s",
                             argv[i], i + 1 < argc ? " " : "");
            if (n < 0) break;
            mi += (size_t)n;
        }
        pop_notification(msg);
        len = shell_appendf(&out, &cap, len, "notified: %s\n", msg);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "head") == 0) {
        /* head [-n N] PATH — first N lines (default 10). N is clamped
         * to [1, 1000] so a malicious script can't ask for 1B lines. */
        int n_lines = 10;
        int first = 1;
        if (argc >= 4 && strcmp(argv[1], "-n") == 0) {
            int v = atoi(argv[2]);
            if (v >= 1 && v <= 1000) n_lines = v;
            first = 3;
        }
        if (argc <= first) {
            *out_text = strdup_safe("head: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        FILE *fp = fopen(argv[first], "r");
        if (!fp) {
            len = shell_appendf(&out, &cap, len,
                                 "head: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char line[4096];
        int emitted = 0;
        while (emitted < n_lines && fgets(line, sizeof(line), fp)) {
            len = shell_appendf(&out, &cap, len, "%s", line);
            emitted++;
        }
        fclose(fp);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "tail") == 0) {
        /* tail [-n N] PATH — last N lines (default 10). For O(file-size)
         * walking we read the whole file (capped 256 KiB) then count
         * newlines backward. Simpler than seeking-back-and-rescanning
         * and more than fast enough for shell-tab use cases. */
        int n_lines = 10;
        int first = 1;
        if (argc >= 4 && strcmp(argv[1], "-n") == 0) {
            int v = atoi(argv[2]);
            if (v >= 1 && v <= 1000) n_lines = v;
            first = 3;
        }
        if (argc <= first) {
            *out_text = strdup_safe("tail: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "tail: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        off_t fsize = lseek(fd, 0, SEEK_END);
        if (fsize < 0) fsize = 0;
        off_t want = fsize;
        if (want > 256 * 1024) want = 256 * 1024;
        if (lseek(fd, fsize - want, SEEK_SET) == (off_t)-1) {
            close(fd);
            *out_text = strdup_safe("tail: seek failed\n");
            *out_exit = 1;
            return 0;
        }
        char *buf = malloc((size_t)want + 1);
        if (!buf) {
            close(fd);
            *out_text = strdup_safe("tail: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t rd = read(fd, buf, (size_t)want);
        close(fd);
        if (rd <= 0) {
            free(buf);
            if (!out) out = strdup_safe("");
            *out_text = out;
            return 0;
        }
        buf[rd] = '\0';
        /* Walk backward N+1 newlines (or fewer = print everything). */
        int seen = 0;
        ssize_t i = rd - 1;
        /* Skip trailing newline so we count "real" line ends. */
        if (i >= 0 && buf[i] == '\n') i--;
        for (; i >= 0; i--) {
            if (buf[i] == '\n') {
                seen++;
                if (seen >= n_lines) { i++; break; }
            }
        }
        if (i < 0) i = 0;
        len = shell_appendf(&out, &cap, len, "%s", buf + i);
        if (rd > 0 && buf[rd - 1] != '\n') {
            len = shell_appendf(&out, &cap, len, "\n");
        }
        free(buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "wc") == 0) {
        /* wc [-l|-c|-w] PATH — line / byte / word counts. Default
         * shows all three. */
        int show_l = 1, show_w = 1, show_c = 1;
        int first = 1;
        if (argc >= 3 && argv[1][0] == '-') {
            show_l = show_w = show_c = 0;
            const char *flags = argv[1] + 1;
            for (const char *f = flags; *f; f++) {
                if (*f == 'l') show_l = 1;
                else if (*f == 'w') show_w = 1;
                else if (*f == 'c') show_c = 1;
            }
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("wc: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "wc: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned long long lines = 0, words = 0, bytes = 0;
        int in_word = 0;
        char chunk[8192];
        ssize_t r;
        while ((r = read(fd, chunk, sizeof(chunk))) > 0) {
            bytes += (unsigned long long)r;
            for (ssize_t k = 0; k < r; k++) {
                unsigned char c = (unsigned char)chunk[k];
                if (c == '\n') lines++;
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                    in_word = 0;
                } else if (!in_word) {
                    in_word = 1;
                    words++;
                }
            }
        }
        close(fd);
        if (show_l) len = shell_appendf(&out, &cap, len, "%8llu", lines);
        if (show_w) len = shell_appendf(&out, &cap, len, "%8llu", words);
        if (show_c) len = shell_appendf(&out, &cap, len, "%8llu", bytes);
        len = shell_appendf(&out, &cap, len, " %s\n", argv[first]);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "which") == 0) {
        /* Look for NAME in a short list of common PS5 dirs that
         * homebrew ELFs land in. PS5 has no system PATH, so this is
         * a convention not a shell-var lookup. */
        if (argc < 2) {
            *out_text = strdup_safe("which: missing NAME\n");
            *out_exit = 1;
            return 0;
        }
        static const char *dirs[] = {
            "/data/bin/", "/data/", "/user/homebrew/bin/",
            "/mnt/usb0/homebrew/bin/", "/system/vsh/app/", NULL,
        };
        int found = 0;
        for (int d = 0; dirs[d]; d++) {
            char p[1024];
            snprintf(p, sizeof(p), "%s%s", dirs[d], argv[1]);
            if (access(p, F_OK) == 0) {
                len = shell_appendf(&out, &cap, len, "%s\n", p);
                found = 1;
            }
        }
        if (!found) {
            len = shell_appendf(&out, &cap, len,
                                 "which: %s: not found\n", argv[1]);
            *out_exit = 1;
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "klog") == 0) {
        /* klog [-n N] — last N bytes from /dev/klog (default 4 KiB,
         * clamped to 64 KiB). Useful for quick kernel-log peeks from
         * shell without leaving the tab for /logs?tab=kernel. */
        size_t n_bytes = 4 * 1024;
        if (argc >= 3 && strcmp(argv[1], "-n") == 0) {
            long v = atol(argv[2]);
            if (v > 0 && v <= 64 * 1024) n_bytes = (size_t)v;
        }
        int fd = open("/dev/klog", O_RDONLY | O_NONBLOCK);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "klog: open /dev/klog: %s\n", strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char *buf = malloc(n_bytes);
        if (!buf) {
            close(fd);
            *out_text = strdup_safe("klog: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t r = read(fd, buf, n_bytes);
        close(fd);
        if (r < 0) r = 0;
        len = shell_appendf(&out, &cap, len, "%.*s", (int)r, buf);
        if (r > 0 && buf[r - 1] != '\n') {
            len = shell_appendf(&out, &cap, len, "\n");
        }
        free(buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "pid") == 0) {
        /* pid NAME — print the pid(s) of processes matching NAME
         * (substring). Saves users a `ps | grep`. */
        if (argc < 2) {
            *out_text = strdup_safe("pid: missing NAME\n");
            *out_exit = 1;
            return 0;
        }
        char *jbuf = malloc(64 * 1024);
        if (!jbuf) {
            *out_text = strdup_safe("pid: oom\n");
            *out_exit = 1;
            return 0;
        }
        size_t jwritten = 0;
        const char *jerr = NULL;
        if (proc_list_get_json(jbuf, 64 * 1024, &jwritten, &jerr) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "pid: %s\n", jerr ? jerr : "failed");
            free(jbuf);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        const char *needle = argv[1];
        int found = 0;
        char *p = jbuf;
        while (p && *p) {
            char *pp = strstr(p, "\"pid\":");
            if (!pp) break;
            int pid_val = atoi(pp + 6);
            char *np = strstr(pp, "\"name\":\"");
            if (!np) break;
            np += 8;
            char *ne = strchr(np, '"');
            if (!ne) break;
            char name_buf[64];
            size_t nl = (size_t)(ne - np);
            if (nl >= sizeof(name_buf)) nl = sizeof(name_buf) - 1;
            memcpy(name_buf, np, nl);
            name_buf[nl] = '\0';
            if (strstr(name_buf, needle)) {
                len = shell_appendf(&out, &cap, len, "%d %s\n",
                                     pid_val, name_buf);
                found++;
            }
            p = ne + 1;
        }
        free(jbuf);
        if (!found) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 2a: file operations ─────────────────────────── */
    if (strcmp(prog, "ln") == 0) {
        /* Symlink only — `ln -s TARGET LINK`. Hardlinks (`ln`) are
         * possible on PS5 but rarely useful since /system is RO. */
        if (argc < 4 || strcmp(argv[1], "-s") != 0) {
            *out_text = strdup_safe("ln: usage: ln -s TARGET LINK\n");
            *out_exit = 1;
            return 0;
        }
        if (symlink(argv[2], argv[3]) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "ln: %s -> %s: %s\n",
                                 argv[3], argv[2], strerror(errno));
            *out_exit = 1;
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "chmod") == 0) {
        /* chmod [-R] MODE PATH... — accepts octal (0644, 644) only.
         * Symbolic mode (u+x etc.) is omitted for simplicity; octal
         * is what most PS5 use-cases want anyway (chmod 755 on a
         * homebrew ELF). */
        int recursive = 0;
        int mode_at = 1;
        if (argc >= 4 && strcmp(argv[1], "-R") == 0) {
            recursive = 1;
            mode_at = 2;
        }
        if (argc <= mode_at + 1) {
            *out_text = strdup_safe("chmod: usage: chmod [-R] MODE PATH...\n");
            *out_exit = 1;
            return 0;
        }
        const char *mode_s = argv[mode_at];
        long mode = strtol(mode_s, NULL, 8);
        /* `chmod 000` is legitimate (clear all bits) so allow mode==0;
         * only reject negative or out-of-range. strtol returns 0 on
         * pure-junk input, so additionally require the first char
         * was a digit to distinguish 0 from "garbage". */
        if (mode < 0 || mode > 07777 ||
            (mode == 0 && (mode_s[0] < '0' || mode_s[0] > '7'))) {
            *out_text = strdup_safe("chmod: invalid octal mode\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = mode_at + 1; i < argc; i++) {
            if (!recursive) {
                if (chmod(argv[i], (mode_t)mode) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", argv[i],
                                         strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* -R: walk via fts. */
            char *paths[2] = { (char *)argv[i], NULL };
            FTS *fts = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "chmod: %s: fts_open: %s\n",
                                     argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DP) continue; /* post-order dirs */
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                if (chmod(ent->fts_accpath, (mode_t)mode) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mv") == 0) {
        /* `mv SRC... DST` — POSIX semantics. If DST is a dir, src is
         * placed inside; otherwise rename. Falls back to copy+delete
         * across filesystems (errno EXDEV). */
        if (argc < 3) {
            *out_text = strdup_safe("mv: usage: mv SRC... DST\n");
            *out_exit = 1;
            return 0;
        }
        const char *dst = argv[argc - 1];
        struct stat dst_st;
        int dst_is_dir = (stat(dst, &dst_st) == 0 && S_ISDIR(dst_st.st_mode));
        int n_src = argc - 2;
        if (n_src > 1 && !dst_is_dir) {
            *out_text = strdup_safe("mv: multi-src requires DST to be a directory\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i <= n_src; i++) {
            char target[1024];
            int tn;
            if (dst_is_dir) {
                const char *base = strrchr(argv[i], '/');
                base = base ? base + 1 : argv[i];
                tn = snprintf(target, sizeof(target), "%s/%s", dst, base);
            } else {
                tn = snprintf(target, sizeof(target), "%s", dst);
            }
            if (tn < 0 || (size_t)tn >= sizeof(target)) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: destination path too long\n", argv[i]);
                any_err = 1;
                continue;
            }
            /* A cross-DEVICE rename() panics this kernel instead of returning
             * EXDEV (see handle_fs_move). Only attempt the rename when source
             * and dest are on the SAME device; otherwise skip straight to the
             * copy-then-unlink path below. Never call rename() across mounts. */
            int mv_same_dev = 0;
            {
                struct stat mv_sf, mv_dd;
                char mv_dpar[1024];
                const char *mv_ds = strrchr(target, '/');
                if (mv_ds && mv_ds != target) {
                    size_t dl = (size_t)(mv_ds - target);
                    if (dl >= sizeof(mv_dpar)) dl = sizeof(mv_dpar) - 1;
                    memcpy(mv_dpar, target, dl);
                    mv_dpar[dl] = '\0';
                } else {
                    mv_dpar[0] = '/';
                    mv_dpar[1] = '\0';
                }
                mv_same_dev = (stat(argv[i], &mv_sf) == 0 &&
                               stat(mv_dpar, &mv_dd) == 0 &&
                               mv_sf.st_dev == mv_dd.st_dev);
            }
            if (mv_same_dev) {
                if (rename(argv[i], target) == 0) continue;
                if (errno != EXDEV) {
                    len = shell_appendf(&out, &cap, len,
                                         "mv: %s -> %s: %s\n",
                                         argv[i], target, strerror(errno));
                    any_err = 1;
                    continue;
                }
            }
            /* Cross-FS — copy then unlink. Single file only; cross-FS
             * directory mv is too complex for shell tab (use cp -r +
             * rm -r explicitly). */
            struct stat sst;
            if (stat(argv[i], &sst) != 0 || !S_ISREG(sst.st_mode)) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s -> %s: cross-FS, only files supported\n",
                                     argv[i], target);
                any_err = 1;
                continue;
            }
            int sfd = open(argv[i], O_RDONLY);
            if (sfd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            int dfd = open(target, O_WRONLY | O_CREAT | O_TRUNC,
                           sst.st_mode & 0777);
            if (dfd < 0) {
                close(sfd);
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: %s\n", target, strerror(errno));
                any_err = 1;
                continue;
            }
            char buf[64 * 1024];
            ssize_t r;
            int copy_err = 0;
            while ((r = read(sfd, buf, sizeof(buf))) > 0) {
                ssize_t off = 0;
                while (off < r) {
                    ssize_t w = write(dfd, buf + off, (size_t)(r - off));
                    if (w <= 0) { copy_err = 1; break; }
                    off += w;
                }
                if (copy_err) break;
            }
            close(sfd);
            close(dfd);
            if (copy_err || r < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s -> %s: copy failed\n",
                                     argv[i], target);
                any_err = 1;
                unlink(target);
                continue;
            }
            if (unlink(argv[i]) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: copied but couldn't unlink: %s\n",
                                     argv[i], strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "cp") == 0) {
        /* `cp [-r] SRC... DST` — file (or recursive dir) copy. Caps
         * single-file size at 256 MiB to avoid OOM (PS5 RAM is
         * tight; users who want bigger transfers should use the
         * Upload tab). */
        int recursive = 0;
        int first_src = 1;
        if (argc >= 4 && strcmp(argv[1], "-r") == 0) {
            recursive = 1;
            first_src = 2;
        }
        if (argc <= first_src + 1) {
            *out_text = strdup_safe("cp: usage: cp [-r] SRC... DST\n");
            *out_exit = 1;
            return 0;
        }
        const char *dst = argv[argc - 1];
        int n_src = argc - first_src - 1;
        struct stat dst_st;
        int dst_is_dir = (stat(dst, &dst_st) == 0 && S_ISDIR(dst_st.st_mode));
        if (n_src > 1 && !dst_is_dir) {
            *out_text = strdup_safe("cp: multi-src requires DST to be a directory\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first_src; i < first_src + n_src; i++) {
            const char *src = argv[i];
            struct stat sst;
            if (stat(src, &sst) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: %s\n", src, strerror(errno));
                any_err = 1;
                continue;
            }
            if (S_ISDIR(sst.st_mode) && !recursive) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: is a directory (use -r)\n", src);
                any_err = 1;
                continue;
            }
            char target[1024];
            int tn;
            if (dst_is_dir) {
                const char *base = strrchr(src, '/');
                base = base ? base + 1 : src;
                tn = snprintf(target, sizeof(target), "%s/%s", dst, base);
            } else {
                tn = snprintf(target, sizeof(target), "%s", dst);
            }
            if (tn < 0 || (size_t)tn >= sizeof(target)) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: destination path too long\n", src);
                any_err = 1;
                continue;
            }
            /* For -r: refuse if TARGET is under SRC. Without this,
             * `cp -r /data /data/copy` would recurse into the newly-
             * created /data/copy and copy it again, etc, until path
             * truncation aborts or the disk fills. Compare via
             * device+inode of every existing ancestor of TARGET
             * against SRC's inode. Target itself doesn't exist yet,
             * so start the walk from the parent. */
            if (recursive && S_ISDIR(sst.st_mode)) {
                struct stat src_st = sst;
                char anc[1024];
                snprintf(anc, sizeof(anc), "%s", target);
                /* Strip the target's leaf to start at its parent. */
                char *slash0 = strrchr(anc, '/');
                if (slash0 == anc) anc[1] = '\0';
                else if (slash0) *slash0 = '\0';
                else snprintf(anc, sizeof(anc), ".");
                int cycle = 0;
                while (1) {
                    struct stat ast;
                    if (stat(anc, &ast) == 0 &&
                        ast.st_dev == src_st.st_dev &&
                        ast.st_ino == src_st.st_ino) {
                        cycle = 1;
                        break;
                    }
                    char *slash = strrchr(anc, '/');
                    if (!slash || slash == anc) break;
                    *slash = '\0';
                }
                if (cycle) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: destination is inside source\n",
                                         src, target);
                    any_err = 1;
                    continue;
                }
            }
            if (!recursive || S_ISREG(sst.st_mode)) {
                /* Single-file copy. */
                if (sst.st_size > 256LL * 1024 * 1024) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %lld bytes exceeds 256 MiB cap "
                                         "— use Upload tab instead\n",
                                         src, (long long)sst.st_size);
                    any_err = 1;
                    continue;
                }
                int sfd = open(src, O_RDONLY);
                if (sfd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", src, strerror(errno));
                    any_err = 1;
                    continue;
                }
                int dfd = open(target, O_WRONLY | O_CREAT | O_TRUNC,
                               sst.st_mode & 0777);
                if (dfd < 0) {
                    close(sfd);
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", target, strerror(errno));
                    any_err = 1;
                    continue;
                }
                char buf[64 * 1024];
                ssize_t r;
                int copy_err = 0;
                while ((r = read(sfd, buf, sizeof(buf))) > 0) {
                    ssize_t off = 0;
                    while (off < r) {
                        ssize_t w = write(dfd, buf + off, (size_t)(r - off));
                        if (w <= 0) { copy_err = 1; break; }
                        off += w;
                    }
                    if (copy_err) break;
                }
                close(sfd);
                close(dfd);
                if (copy_err || r < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: copy failed\n",
                                         src, target);
                    any_err = 1;
                    unlink(target);
                }
                continue;
            }
            /* Recursive directory copy via FTS. */
            char *paths[2] = { (char *)src, NULL };
            FTS *fts = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: fts_open: %s\n",
                                     src, strerror(errno));
                any_err = 1;
                continue;
            }
            size_t src_prefix_len = strlen(src);
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DP) continue;
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                /* dst_path = target + (fts_path - src) */
                const char *rel = ent->fts_path + src_prefix_len;
                while (*rel == '/') rel++;
                char dpath[1024];
                int dn;
                if (*rel)
                    dn = snprintf(dpath, sizeof(dpath), "%s/%s", target, rel);
                else
                    dn = snprintf(dpath, sizeof(dpath), "%s", target);
                if (dn < 0 || (size_t)dn >= sizeof(dpath)) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: path too long\n", ent->fts_path);
                    any_err = 1;
                    continue;
                }
                if (ent->fts_info == FTS_D) {
                    if (mkdir(dpath, ent->fts_statp->st_mode & 0777) != 0
                        && errno != EEXIST) {
                        len = shell_appendf(&out, &cap, len,
                                             "cp: %s: mkdir: %s\n",
                                             dpath, strerror(errno));
                        any_err = 1;
                    }
                    continue;
                }
                if (ent->fts_info != FTS_F) continue;
                /* File copy — same byte loop as the single-file path. */
                int sfd = open(ent->fts_accpath, O_RDONLY);
                if (sfd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                    continue;
                }
                int dfd = open(dpath, O_WRONLY | O_CREAT | O_TRUNC,
                               ent->fts_statp->st_mode & 0777);
                if (dfd < 0) {
                    close(sfd);
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", dpath, strerror(errno));
                    any_err = 1;
                    continue;
                }
                char buf[64 * 1024];
                ssize_t rd;
                while ((rd = read(sfd, buf, sizeof(buf))) > 0) {
                    ssize_t off = 0;
                    while (off < rd) {
                        ssize_t w = write(dfd, buf + off, (size_t)(rd - off));
                        if (w <= 0) { rd = -1; break; }
                        off += w;
                    }
                }
                close(sfd);
                close(dfd);
                if (rd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: copy failed\n",
                                         ent->fts_path, dpath);
                    any_err = 1;
                    unlink(dpath);
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "rm") == 0) {
        /* `rm [-r] [-f] PATH...` — refuses /system* and /preinst* to
         * avoid bricking the console. -f silences "missing operand"
         * errors AND ENOENT but does NOT suppress permission errors
         * (these are real bugs the user wants to know about). */
        int recursive = 0;
        int force = 0;
        int first = 1;
        while (first < argc && argv[first][0] == '-') {
            for (const char *f = argv[first] + 1; *f; f++) {
                if (*f == 'r' || *f == 'R') recursive = 1;
                else if (*f == 'f') force = 1;
            }
            first++;
        }
        if (argc <= first) {
            if (!force) {
                *out_text = strdup_safe("rm: missing operand\n");
                *out_exit = 1;
                return 0;
            }
            *out_text = strdup_safe("");
            return 0;
        }
        int any_err = 0;
        for (int i = first; i < argc; i++) {
            const char *p = argv[i];
            /* Trip-wire on system paths. /system + /system_ex +
             * /preinst + /preinst_ex are all Sony-mounted ro and
             * recursive rm would just spam errors; refusing here
             * surfaces an actionable message.
             *
             * Normalize p first so `//system/foo` or `/./system/foo`
             * can't slip past the prefix check. Naive normalize:
             * collapse leading `//+` and `/./` into `/`. */
            char norm[1024];
            {
                size_t ni = 0;
                size_t pi = 0;
                while (p[pi] && ni + 1 < sizeof(norm)) {
                    if (p[pi] == '/') {
                        norm[ni++] = '/';
                        while (p[pi] == '/' ||
                               (p[pi] == '/' && p[pi+1] == '.' &&
                                (p[pi+2] == '/' || p[pi+2] == '\0'))) {
                            if (p[pi] == '/' && p[pi+1] == '.' &&
                                (p[pi+2] == '/' || p[pi+2] == '\0')) pi += 2;
                            else pi++;
                        }
                    } else {
                        norm[ni++] = p[pi++];
                    }
                }
                norm[ni] = '\0';
            }
            static const char *banned[] = {
                "/", "/system", "/system_ex", "/preinst", "/preinst_ex",
            };
            int refused = 0;
            for (size_t b = 0; b < sizeof(banned) / sizeof(banned[0]); b++) {
                size_t bl = strlen(banned[b]);
                if (strcmp(norm, banned[b]) == 0) { refused = 1; break; }
                if (strncmp(norm, banned[b], bl) == 0 && norm[bl] == '/') {
                    refused = 1;
                    break;
                }
            }
            if (refused) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: refusing to touch system path\n", p);
                any_err = 1;
                continue;
            }
            struct stat sst;
            if (lstat(p, &sst) != 0) {
                if (!force) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", p, strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            if (S_ISDIR(sst.st_mode) && !recursive) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: is a directory (use -r)\n", p);
                any_err = 1;
                continue;
            }
            if (!S_ISDIR(sst.st_mode)) {
                if (unlink(p) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", p, strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* Recursive directory remove via FTS post-order. */
            char *paths[2] = { (char *)p, NULL };
            FTS *fts = fts_open(paths,
                                 FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: fts_open: %s\n", p,
                                     strerror(errno));
                any_err = 1;
                continue;
            }
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                if (ent->fts_info == FTS_D) continue; /* pre-order */
                if (ent->fts_info == FTS_DP) {
                    if (rmdir(ent->fts_accpath) != 0) {
                        len = shell_appendf(&out, &cap, len,
                                             "rm: %s: %s\n", ent->fts_path,
                                             strerror(errno));
                        any_err = 1;
                    }
                    continue;
                }
                if (unlink(ent->fts_accpath) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 2b: search + inspect ───────────────────────── */
    if (strcmp(prog, "find") == 0) {
        /* `find [PATH] [-name GLOB] [-type f|d|l]` — walk PATH (default
         * .) and print entries matching the filters. Cap at 10000
         * entries to stay within 256 KiB response budget. */
        const char *path = ".";
        const char *name_glob = NULL;
        char type_filter = 0;
        int i = 1;
        if (i < argc && argv[i][0] != '-') {
            path = argv[i++];
        }
        while (i < argc) {
            if (i + 1 < argc && strcmp(argv[i], "-name") == 0) {
                name_glob = argv[i + 1];
                i += 2;
            } else if (i + 1 < argc && strcmp(argv[i], "-type") == 0) {
                type_filter = argv[i + 1][0];
                i += 2;
            } else {
                len = shell_appendf(&out, &cap, len,
                                     "find: unknown arg %s\n", argv[i]);
                *out_exit = 1;
                *out_text = out;
                return 0;
            }
        }
        char *paths[2] = { (char *)path, NULL };
        FTS *fts_h = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
        if (!fts_h) {
            len = shell_appendf(&out, &cap, len,
                                 "find: %s: %s\n", path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        int count = 0;
        int truncated = 0;
        FTSENT *ent;
        while ((ent = fts_read(fts_h)) != NULL) {
            if (ent->fts_info == FTS_DP) continue;
            if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) continue;
            if (type_filter) {
                char t = 0;
                if (ent->fts_info == FTS_D) t = 'd';
                else if (ent->fts_info == FTS_F) t = 'f';
                else if (ent->fts_info == FTS_SL || ent->fts_info == FTS_SLNONE) t = 'l';
                if (t != type_filter) continue;
            }
            if (name_glob) {
                const char *base = strrchr(ent->fts_path, '/');
                base = base ? base + 1 : ent->fts_path;
                if (fnmatch(name_glob, base, 0) != 0) continue;
            }
            len = shell_appendf(&out, &cap, len, "%s\n", ent->fts_path);
            count++;
            if (count >= 10000) {
                truncated = 1;
                break;
            }
        }
        fts_close(fts_h);
        if (truncated) {
            len = shell_appendf(&out, &cap, len,
                                 "(... find result capped at 10000 entries; "
                                 "narrow with -name or -type)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "grep") == 0) {
        /* `grep [-r] [-i] [-E] [-l] [-c] PATTERN PATH...` — POSIX
         * regex via regex.h. -r walks dirs via FTS. -l prints only
         * matching filenames. -c prints only counts. Caps output at
         * 5000 match lines. */
        int recursive = 0, case_i = 0, list_only = 0, count_only = 0;
        int i = 1;
        while (i < argc && argv[i][0] == '-' && argv[i][1]) {
            for (const char *f = argv[i] + 1; *f; f++) {
                if (*f == 'r' || *f == 'R') recursive = 1;
                else if (*f == 'i') case_i = 1;
                else if (*f == 'E') { /* default; accepted */ }
                else if (*f == 'l') list_only = 1;
                else if (*f == 'c') count_only = 1;
                else {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: bad flag -%c\n", *f);
                    *out_exit = 1;
                    *out_text = out;
                    return 0;
                }
            }
            i++;
        }
        if (i >= argc) {
            *out_text = strdup_safe("grep: usage: grep [-riElc] PATTERN PATH...\n");
            *out_exit = 1;
            return 0;
        }
        const char *pat = argv[i++];
        regex_t re;
        int flags = REG_EXTENDED | (case_i ? REG_ICASE : 0);
        int rrc = regcomp(&re, pat, flags);
        if (rrc != 0) {
            char errbuf[256];
            regerror(rrc, &re, errbuf, sizeof(errbuf));
            len = shell_appendf(&out, &cap, len,
                                 "grep: bad pattern: %s\n", errbuf);
            *out_exit = 1;
            *out_text = out;
            return 0;
        }
        if (i >= argc) {
            regfree(&re);
            *out_text = strdup_safe("grep: missing PATH (stdin not supported)\n");
            *out_exit = 1;
            return 0;
        }
        int total_matches = 0;
        int total_capped = 0;
        for (; i < argc && !total_capped; i++) {
            const char *p = argv[i];
            struct stat st_p;
            int is_dir = (stat(p, &st_p) == 0 && S_ISDIR(st_p.st_mode));
            char *paths[2] = { (char *)p, NULL };
            FTS *fts_h = NULL;
            if (recursive && is_dir) {
                fts_h = fts_open(paths,
                                FTS_PHYSICAL | FTS_NOCHDIR, NULL);
                if (!fts_h) {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: %s: %s\n", p, strerror(errno));
                    continue;
                }
            } else if (is_dir) {
                len = shell_appendf(&out, &cap, len,
                                     "grep: %s: is a directory (use -r)\n", p);
                continue;
            }
            const char *next_path = NULL;
            FTSENT *ent = NULL;
            while (1) {
                if (fts_h) {
                    ent = fts_read(fts_h);
                    if (!ent) break;
                    if (ent->fts_info != FTS_F) continue;
                    next_path = ent->fts_path;
                } else {
                    if (next_path) break;
                    next_path = p;
                }
                FILE *fp = fopen(next_path, "r");
                if (!fp) {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: %s: %s\n", next_path, strerror(errno));
                    if (!fts_h) break;
                    continue;
                }
                char line[8192];
                int file_match_count = 0;
                while (fgets(line, sizeof(line), fp)) {
                    size_t L = strlen(line);
                    if (L > 0 && line[L - 1] == '\n') line[L - 1] = '\0';
                    if (regexec(&re, line, 0, NULL, 0) != 0) continue;
                    file_match_count++;
                    total_matches++;
                    if (!list_only && !count_only) {
                        len = shell_appendf(&out, &cap, len, "%s%s%s\n",
                                             (recursive && fts_h) ? next_path : "",
                                             (recursive && fts_h) ? ":" : "",
                                             line);
                    }
                    if (list_only) break;
                    if (total_matches >= 5000) {
                        total_capped = 1;
                        break;
                    }
                }
                fclose(fp);
                if (list_only && file_match_count > 0) {
                    len = shell_appendf(&out, &cap, len, "%s\n", next_path);
                }
                if (count_only) {
                    if (recursive && fts_h) {
                        len = shell_appendf(&out, &cap, len, "%s:%d\n",
                                             next_path, file_match_count);
                    } else {
                        len = shell_appendf(&out, &cap, len, "%d\n",
                                             file_match_count);
                    }
                }
                if (!fts_h) break;
                if (total_capped) break;
            }
            if (fts_h) fts_close(fts_h);
        }
        regfree(&re);
        if (total_matches == 0 && !count_only) *out_exit = 1;
        if (total_capped) {
            len = shell_appendf(&out, &cap, len,
                                 "(... grep result capped at 5000 matches)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "du") == 0) {
        /* `du [-sh] PATH...` — summary in human-readable units. */
        int human = 0;
        int i = 1;
        if (i < argc && argv[i][0] == '-') {
            for (const char *f = argv[i] + 1; *f; f++) {
                if (*f == 'h') human = 1;
                else if (*f == 's') { /* default; accepted */ }
            }
            i++;
        }
        if (i >= argc) {
            *out_text = strdup_safe("du: usage: du [-sh] PATH...\n");
            *out_exit = 1;
            return 0;
        }
        for (; i < argc; i++) {
            char *paths[2] = { (char *)argv[i], NULL };
            FTS *fts_h = fts_open(paths,
                                 FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts_h) {
                len = shell_appendf(&out, &cap, len,
                                     "du: %s: %s\n", argv[i], strerror(errno));
                *out_exit = 1;
                continue;
            }
            unsigned long long total_bytes = 0;
            FTSENT *ent;
            while ((ent = fts_read(fts_h)) != NULL) {
                if (ent->fts_info == FTS_DP) continue;
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) continue;
                if (ent->fts_info == FTS_F) {
                    total_bytes += (unsigned long long)ent->fts_statp->st_size;
                }
            }
            fts_close(fts_h);
            if (human) {
                const char *unit = "B";
                double v = (double)total_bytes;
                if (v >= 1024) { v /= 1024; unit = "K"; }
                if (v >= 1024) { v /= 1024; unit = "M"; }
                if (v >= 1024) { v /= 1024; unit = "G"; }
                if (v >= 1024) { v /= 1024; unit = "T"; }
                len = shell_appendf(&out, &cap, len, "%.1f%s\t%s\n",
                                     v, unit, argv[i]);
            } else {
                len = shell_appendf(&out, &cap, len, "%llu\t%s\n",
                                     total_bytes / 1024, argv[i]);
            }
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "xxd") == 0 || strcmp(prog, "hexdump") == 0) {
        /* `xxd PATH` — canonical `hexdump -C` layout. 16 bytes per
         * row, offset + hex + ASCII. Cap at 16 KiB. */
        int first = 1;
        if (strcmp(prog, "hexdump") == 0 && argc >= 2 &&
            strcmp(argv[1], "-C") == 0) {
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("xxd: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "xxd: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned char chunk[16];
        long offset = 0;
        ssize_t r;
        long total = 0;
        while (total < 16 * 1024 && (r = read(fd, chunk, sizeof(chunk))) > 0) {
            len = shell_appendf(&out, &cap, len, "%08lx  ", offset);
            for (int k = 0; k < 16; k++) {
                if (k < r) {
                    len = shell_appendf(&out, &cap, len, "%02x ", chunk[k]);
                } else {
                    len = shell_appendf(&out, &cap, len, "   ");
                }
                if (k == 7) {
                    len = shell_appendf(&out, &cap, len, " ");
                }
            }
            len = shell_appendf(&out, &cap, len, " |");
            for (int k = 0; k < r; k++) {
                unsigned char c = chunk[k];
                len = shell_appendf(&out, &cap, len, "%c",
                                     (c >= 0x20 && c < 0x7f) ? c : '.');
            }
            len = shell_appendf(&out, &cap, len, "|\n");
            offset += r;
            total += r;
        }
        close(fd);
        if (total >= 16 * 1024) {
            len = shell_appendf(&out, &cap, len,
                                 "(... xxd output capped at 16 KiB)\n");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "file") == 0) {
        /* Magic-byte detection for PS5-relevant file types. */
        if (argc < 2) {
            *out_text = strdup_safe("file: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            int fd = open(argv[i], O_RDONLY);
            if (fd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "%s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            unsigned char m[32] = {0};
            ssize_t r = read(fd, m, sizeof(m));
            close(fd);
            const char *kind = "data";
            if (r >= 4) {
                if (m[0] == 0x4f && m[1] == 0x15 && m[2] == 0x3d && m[3] == 0x1d)
                    kind = "PS5 SELF (Sony-signed)";
                else if (m[0] == 0x7f && m[1] == 'E' && m[2] == 'L' && m[3] == 'F')
                    kind = "ELF executable";
                else if (m[0] == 0x7f && m[1] == 'P' && m[2] == 'R' && m[3] == 'X')
                    kind = "PRX library";
                else if (r >= 8 && m[0] == 'S' && m[1] == 'C' && m[2] == 'E' &&
                         m[3] == 'U' && m[4] == 'F')
                    kind = "PS5 PUP firmware update";
                else if (m[0] == 0x7f && m[1] == 'C' && m[2] == 'N' && m[3] == 'T')
                    kind = "PS5 PKG content package";
                else if (m[0] == 0x89 && m[1] == 'P' && m[2] == 'N' && m[3] == 'G')
                    kind = "PNG image";
                else if (r >= 3 && m[0] == 0xff && m[1] == 0xd8 && m[2] == 0xff)
                    kind = "JPEG image";
                else if (m[0] == 'P' && m[1] == 'K' && m[2] == 0x03 && m[3] == 0x04)
                    kind = "ZIP archive";
                else if (m[0] == 0x00 && m[1] == 'P' && m[2] == 'S' && m[3] == 'F')
                    kind = "param.sfo metadata";
                else if (r >= 8 && m[0] == 0 && m[1] == 0 && m[2] == 0 &&
                         m[3] == 0 && m[4] == 'M' && m[5] == 'O' && m[6] == 'V')
                    kind = "MOV/MP4 video";
                else {
                    int printable = 1;
                    for (ssize_t k = 0; k < r; k++) {
                        unsigned char c = m[k];
                        if (c < 0x09 || (c > 0x0d && c < 0x20) || c >= 0x7f) {
                            printable = 0;
                            break;
                        }
                    }
                    if (printable && r > 0) kind = "ASCII text";
                }
            } else if (r == 0) {
                kind = "empty";
            }
            len = shell_appendf(&out, &cap, len, "%s: %s\n", argv[i], kind);
        }
        if (any_err) *out_exit = 1;
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 3: PS5-specific niche ───────────────────────── */
    if (strcmp(prog, "mtrw") == 0) {
        /* Remount /system (or arbitrary mount) read-write — one of the
         * most-asked PS5 verbs. Sony mounts /system + /system_ex
         * read-only; turning them rw lets users patch system
         * resources or install custom UI assets. Requires kernel R/W
         * (kstuff) — otherwise nmount returns EACCES.
         *
         * Usage: `mtrw` (= /system), `mtrw /system_ex`, `mtrw /preinst`. */
        const char *mnt = argc >= 2 ? argv[1] : "/system";
        /* nmount(MNT_UPDATE) with the iovec containing the mount
         * point + fstype keeps the same fs but flips rdonly. The
         * "fstype" must match what's already mounted there (ufs or
         * nullfs typically); we look it up via getmntinfo. */
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        const char *fstype = NULL;
        const char *from = NULL;
        for (int i = 0; i < n && mnts; i++) {
            if (strcmp(mnts[i].f_mntonname, mnt) == 0) {
                fstype = mnts[i].f_fstypename;
                from = mnts[i].f_mntfromname;
                break;
            }
        }
        /* NOTE: fstype/from alias INTO `mnts`, so the snapshot must stay alive
         * until after the nmount iovec below is built and used — free it only
         * on each return path past that point (and here, where they're unused). */
        if (!fstype) {
            len = shell_appendf(&out, &cap, len,
                                 "mtrw: %s: not a mounted filesystem\n", mnt);
            *out_text = out;
            *out_exit = 1;
            free(mnts);
            return 0;
        }
        /* nmount(2) iovec: each option name + value pair, iov_len
         * INCLUDES the trailing NUL byte (per man page). `from`
         * comes from the existing mount's f_mntfromname so the
         * kernel matches the underlying device/source correctly. */
        struct iovec iov[6];
        iov[0].iov_base = (void *)"fstype";
        iov[0].iov_len = strlen("fstype") + 1;
        iov[1].iov_base = (void *)fstype;
        iov[1].iov_len = strlen(fstype) + 1;
        iov[2].iov_base = (void *)"fspath";
        iov[2].iov_len = strlen("fspath") + 1;
        iov[3].iov_base = (void *)mnt;
        iov[3].iov_len = strlen(mnt) + 1;
        iov[4].iov_base = (void *)"from";
        iov[4].iov_len = strlen("from") + 1;
        iov[5].iov_base = (void *)from;
        iov[5].iov_len = strlen(from) + 1;
        if (nmount(iov, 6, MNT_UPDATE) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "mtrw: %s: %s (need kernel R/W via kstuff?)\n",
                                 mnt, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            free(mnts); /* last use of `from` (iov[5]) was above */
            return 0;
        }
        len = shell_appendf(&out, &cap, len, "%s remounted rw (%s)\n",
                             mnt, fstype);
        *out_text = out;
        free(mnts); /* last use of `fstype` was the line above */
        return 0;
    }
    if (strcmp(prog, "sfoinfo") == 0) {
        /* `sfoinfo PATH` — parse SCE param.sfo and print key/value
         * pairs. Format:
         *   magic    0x00 0x50 0x53 0x46 (\x00PSF)
         *   version  uint32 LE (usually 0x01010000)
         *   k_table  uint32 LE — offset to key table (UTF-8 names)
         *   d_table  uint32 LE — offset to data table (values)
         *   n_entries uint32 LE
         *   entries[n_entries]: {
         *      uint16 LE key_off (into k_table)
         *      uint8  align
         *      uint8  fmt    (0x04=utf8, 0x02=utf8-sz, 0x04=int32)
         *      uint32 LE used_size
         *      uint32 LE total_size
         *      uint32 LE data_off (into d_table)
         *   }
         * We read the whole file (cap 64 KiB) into memory and parse
         * in-place. */
        if (argc < 2) {
            *out_text = strdup_safe("sfoinfo: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[1], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: %s\n", argv[1], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned char *sfo = malloc(64 * 1024);
        if (!sfo) {
            close(fd);
            *out_text = strdup_safe("sfoinfo: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t r = read(fd, sfo, 64 * 1024);
        close(fd);
        if (r < 20 || sfo[0] != 0x00 || sfo[1] != 'P' || sfo[2] != 'S' ||
            sfo[3] != 'F') {
            free(sfo);
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: not a SFO file\n", argv[1]);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        /* little-endian uint32 read */
        #define LE32(off) ((uint32_t)sfo[off] | ((uint32_t)sfo[off+1] << 8) |    \
                          ((uint32_t)sfo[off+2] << 16) | ((uint32_t)sfo[off+3] << 24))
        uint32_t k_table = LE32(8);
        uint32_t d_table = LE32(12);
        uint32_t n = LE32(16);
        if (n > 256 || k_table >= (uint32_t)r || d_table >= (uint32_t)r) {
            free(sfo);
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: corrupt SFO header\n", argv[1]);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        uint32_t r32 = (uint32_t)r;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t e_off = 20 + i * 16;
            if (e_off + 16 > r32) break;
            uint16_t k_off = (uint16_t)(sfo[e_off] | (sfo[e_off+1] << 8));
            uint8_t fmt    = sfo[e_off + 3];
            uint32_t used  = LE32(e_off + 4);
            uint32_t d_off = LE32(e_off + 12);
            /* Bounds checks — attacker-controlled offsets from the
             * file, so do all arithmetic as overflow-safe
             * subtractions: `a + b > r` becomes `b > r - a` after
             * verifying a <= r. CRIT audit caught this on 2.13.0. */
            if (k_off > r32 || k_table > r32 - k_off) break;
            if (d_table > r32) break;
            if (d_off > r32 - d_table) break;
            if (used > r32 - d_table - d_off) break;
            const char *key = (const char *)(sfo + k_table + k_off);
            const unsigned char *data = sfo + d_table + d_off;
            /* `key` is read as a NUL-terminated string by `%s` —
             * verify a NUL exists before end-of-buffer. memchr
             * walks at most r-(k_table+k_off) bytes and bails. */
            uint32_t key_max = r32 - k_table - k_off;
            if (!memchr(key, '\0', key_max)) break;
            len = shell_appendf(&out, &cap, len, "%-24s = ", key);
            if (fmt == 0x04 && used == 4) {
                /* int32 */
                uint32_t v = (uint32_t)data[0] | ((uint32_t)data[1] << 8) |
                              ((uint32_t)data[2] << 16) | ((uint32_t)data[3] << 24);
                len = shell_appendf(&out, &cap, len, "%u (0x%08x)\n", v, v);
            } else {
                /* utf-8 string — used may include trailing NUL */
                size_t print_len = used;
                if (print_len > 0 && data[print_len - 1] == 0) print_len--;
                /* Be defensive about non-printables. */
                int ok = 1;
                for (size_t k = 0; k < print_len; k++) {
                    if (data[k] < 0x09 ||
                        (data[k] > 0x0d && data[k] < 0x20)) {
                        ok = 0;
                        break;
                    }
                }
                if (ok) {
                    len = shell_appendf(&out, &cap, len, "\"%.*s\"\n",
                                         (int)print_len, data);
                } else {
                    len = shell_appendf(&out, &cap, len, "(binary, %u bytes)\n",
                                         used);
                }
            }
        }
        #undef LE32
        free(sfo);
        *out_text = out;
        return 0;
    }
    /* Unknown command — let the caller decide whether to fall through
     * to popen or surface a "not supported" error. */
    (void)len;
    if (out) free(out);
    return -1;
}

static int handle_shell_exec(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    char cmd[2048] = {0};
    char cwd[1024] = "/";
    char fallback_cwd[1024] = "/";
    char session_id[96] = "";
    int timeout_secs = 30;
    if (body && body_len > 0 && body_len < 4096) {
        char tmp[4100];
        memcpy(tmp, body, (size_t)body_len);
        tmp[body_len] = '\0';
        (void)shell_json_string_field(tmp, body_len, "cmd", cmd, sizeof(cmd));
        if (shell_json_string_field(tmp, body_len, "cwd", fallback_cwd, sizeof(fallback_cwd)) != 0 ||
            fallback_cwd[0] != '/') {
            snprintf(fallback_cwd, sizeof(fallback_cwd), "/");
        }
        (void)shell_json_string_field(tmp, body_len, "session_id",
                                      session_id, sizeof(session_id));
        const char *p = strstr(tmp, "\"timeout_secs\"");
        if (p) {
            p = strchr(p, ':');
            if (p) {
                int n = atoi(p + 1);
                if (n > 0 && n < 600) timeout_secs = n;
            }
        }
    }
    shell_session_get(session_id, fallback_cwd, cwd, sizeof(cwd));
    if (cmd[0] == '\0') {
        const char *err = "{\"err\":\"empty_cmd\"}";
        return send_frame(client_fd, FTX2_FRAME_SHELL_EXEC_ACK, 0,
                          trace_id, err, strlen(err));
    }
    {
        char probe[2100];
        char *argv_probe[8];
        snprintf(probe, sizeof(probe), "%s", cmd);
        int argc_probe = shell_split(probe, argv_probe, 8);
        if (argc_probe > 0 && strcmp(argv_probe[0], "pwd") == 0) {
            char out[1100];
            snprintf(out, sizeof(out), "%s\n", cwd);
            return shell_send_json_result(state, client_fd, trace_id, 0, out, cwd, session_id, 0);
        }
        if (argc_probe > 0 && strcmp(argv_probe[0], "cd") == 0) {
            const char *target = argc_probe >= 2 ? argv_probe[1] : "/";
            char resolved[1024];
            char err[160];
            if (shell_resolve_dir(cwd, target, resolved, sizeof(resolved),
                                  err, sizeof(err)) != 0) {
                char out[1400];
                snprintf(out, sizeof(out), "cd: %s: %s\n", target, err);
                return shell_send_json_result(state, client_fd, trace_id, 1, out, cwd, session_id, 0);
            }
            shell_session_set(session_id, resolved);
            snprintf(cwd, sizeof(cwd), "%s", resolved);
            return shell_send_json_result(state, client_fd, trace_id, 0, "", cwd, session_id, 0);
        }
        if (argc_probe > 0 && strcmp(argv_probe[0], "ls") == 0 && argc_probe <= 2) {
            const char *target = argc_probe >= 2 ? argv_probe[1] : cwd;
            char abs_path[1024];
            char err[160];
            if (shell_resolve_dir(cwd, target, abs_path, sizeof(abs_path),
                                  err, sizeof(err)) != 0) {
                char out[1400];
                snprintf(out, sizeof(out), "ls: %s: %s\n", target, err);
                return shell_send_json_result(state, client_fd, trace_id, 1, out, cwd, session_id, 0);
            }
            char *builtin_out = NULL;
            int builtin_exit = -1;
            int builtin_rc = shell_ls_path(abs_path, &builtin_out, &builtin_exit);
            if (builtin_rc == 0) {
                int rc = shell_send_json_result(state, client_fd, trace_id,
                                                builtin_exit,
                                                builtin_out ? builtin_out : "",
                                                cwd,
                                                session_id,
                                                0);
                free(builtin_out);
                return rc;
            }
            free(builtin_out);
        }
    }
    /* PS5 has NO shell binary — there's no /bin/sh, /system/bin/sh, or
     * any other executable popen() could fork+exec. Skip the popen
     * call entirely and route every command through our built-in
     * interpreter (handle_shell_builtin) which uses the payload's
     * existing FS/proc/uname code paths to answer the most-common
     * "what's the state of this PS5?" queries. */
    {
        char *builtin_out = NULL;
        int builtin_exit = -1;
        int builtin_rc = -1;

        pthread_mutex_lock(&g_shell_cwd_mtx);
        char saved_cwd[1024];
        int have_saved_cwd = getcwd(saved_cwd, sizeof(saved_cwd)) != NULL;
        if (cwd[0] && chdir(cwd) != 0) {
            size_t cap = 0, len = 0;
            builtin_exit = 1;
            len = shell_appendf(&builtin_out, &cap, len,
                                "shell: cwd %s: %s\n", cwd, strerror(errno));
            (void)len;
            builtin_rc = 0;
        } else {
            builtin_rc = handle_shell_builtin(cmd, &builtin_out, &builtin_exit);
        }
        if (have_saved_cwd) {
            (void)chdir(saved_cwd);
        } else {
            (void)chdir("/");
        }
        pthread_mutex_unlock(&g_shell_cwd_mtx);

        if (builtin_rc == 0) {
            (void)timeout_secs;
            int rc_b = shell_send_json_result(state, client_fd, trace_id,
                                              builtin_exit,
                                              builtin_out ? builtin_out : "",
                                              cwd,
                                              session_id,
                                              0);
            free(builtin_out);
            return rc_b;
        }
        free(builtin_out);
    }

    (void)timeout_secs;
    char msg[512];
    snprintf(msg, sizeof(msg),
             "%s: command not found. PS5Upload shell only supports built-ins; type 'help'.\n",
             cmd[0] ? cmd : "shell");
    return shell_send_json_result(state, client_fd, trace_id, 127, msg, cwd, session_id, 0);
}

/* ── CRC32 file checksum ─────────────────────────────────────────────── */

static uint32_t g_crc32_table[256];
static int g_crc32_table_built = 0;
static void build_crc32_table(void) {
    if (g_crc32_table_built) return;
    for (int i = 0; i < 256; i++) {
        uint32_t c = (uint32_t)i;
        for (int j = 0; j < 8; j++) {
            c = (c & 1) ? (0xedb88320u ^ (c >> 1)) : (c >> 1);
        }
        g_crc32_table[i] = c;
    }
    g_crc32_table_built = 1;
}

static int handle_crc32_file(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    char path[1024] = {0};
    if (body && body_len > 0 && body_len < 1024) {
        char tmp[1028];
        memcpy(tmp, body, (size_t)body_len);
        tmp[body_len] = '\0';
        const char *p = strstr(tmp, "\"path\"");
        if (p) {
            p = strchr(p, ':');
            if (p) p = strchr(p, '"');
            if (p) {
                p++;
                const char *e = strchr(p, '"');
                if (e && (size_t)(e - p) < sizeof(path)) {
                    memcpy(path, p, (size_t)(e - p));
                    path[e - p] = '\0';
                }
            }
        }
    }
    if (path[0] == '\0') {
        const char *err = "{\"err\":\"empty_path\"}";
        return send_frame(client_fd, FTX2_FRAME_CRC32_FILE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        const char *err = "{\"err\":\"open_failed\"}";
        return send_frame(client_fd, FTX2_FRAME_CRC32_FILE_ACK, 0,
                          trace_id, err, strlen(err));
    }
    build_crc32_table();
    uint32_t crc = 0xffffffffu;
    uint64_t total = 0;
    char buf[64 * 1024];
    while (1) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n <= 0) break;
        for (ssize_t i = 0; i < n; i++) {
            crc = g_crc32_table[(crc ^ (unsigned char)buf[i]) & 0xff]
                  ^ (crc >> 8);
        }
        total += (uint64_t)n;
    }
    close(fd);
    crc ^= 0xffffffffu;
    char resp[160];
    int rn = snprintf(resp, sizeof(resp),
                      "{\"crc32\":%u,\"size\":%llu}",
                      crc, (unsigned long long)total);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_CRC32_FILE_ACK, 0,
                      trace_id, resp, (uint64_t)rn);
}

/* ── app.db query ────────────────────────────────────────────────────── */

/* sqlite functions resolved via RTLD_DEFAULT — same pattern as
 * register.c's resolution. We re-resolve here rather than hand
 * pointers across module boundaries because handle_appdb_query is
 * the only sqlite consumer outside register.c and we want the
 * runtime path to fail gracefully if libsqlite has moved (firmware
 * delta) without propagating that failure into other handlers. */
typedef struct sqlite3      sqlite3;
typedef struct sqlite3_stmt sqlite3_stmt;
typedef int  (*adb_open_v2_fn)(const char *, sqlite3 **, int, const char *);
typedef int  (*adb_close_fn)(sqlite3 *);
typedef int  (*adb_prepare_v2_fn)(sqlite3 *, const char *, int,
                                   sqlite3_stmt **, const char **);
typedef int  (*adb_step_fn)(sqlite3_stmt *);
typedef int  (*adb_finalize_fn)(sqlite3_stmt *);
typedef const unsigned char *(*adb_column_text_fn)(sqlite3_stmt *, int);
typedef int  (*adb_column_int_fn)(sqlite3_stmt *, int);
#define ADB_SQLITE_OPEN_READONLY 0x00000001
#define ADB_SQLITE_ROW 100

static int handle_appdb_query(runtime_state_t *state, int client_fd,
                               uint64_t trace_id) {
    if (!state) return -1;
    adb_open_v2_fn       sq_open_v2  = (adb_open_v2_fn)dlsym(RTLD_DEFAULT, "sqlite3_open_v2");
    adb_close_fn         sq_close    = (adb_close_fn)dlsym(RTLD_DEFAULT, "sqlite3_close");
    adb_prepare_v2_fn    sq_prepare  = (adb_prepare_v2_fn)dlsym(RTLD_DEFAULT, "sqlite3_prepare_v2");
    adb_step_fn          sq_step     = (adb_step_fn)dlsym(RTLD_DEFAULT, "sqlite3_step");
    adb_finalize_fn      sq_finalize = (adb_finalize_fn)dlsym(RTLD_DEFAULT, "sqlite3_finalize");
    adb_column_text_fn   sq_text     = (adb_column_text_fn)dlsym(RTLD_DEFAULT, "sqlite3_column_text");
    adb_column_int_fn    sq_int      = (adb_column_int_fn)dlsym(RTLD_DEFAULT, "sqlite3_column_int");
    if (!sq_open_v2 || !sq_close || !sq_prepare || !sq_step ||
        !sq_finalize || !sq_text || !sq_int) {
        const char *err = "{\"err\":\"sqlite_unavailable\",\"apps\":[]}";
        return send_frame(client_fd, FTX2_FRAME_APPDB_QUERY_ACK, 0,
                          trace_id, err, strlen(err));
    }
    sqlite3 *db = NULL;
    int rc = sq_open_v2("/system_data/priv/mms/app.db",
                         &db, ADB_SQLITE_OPEN_READONLY, NULL);
    if (rc != 0 || !db) {
        if (db) sq_close(db);
        const char *err = "{\"err\":\"open_appdb_failed\",\"apps\":[]}";
        return send_frame(client_fd, FTX2_FRAME_APPDB_QUERY_ACK, 0,
                          trace_id, err, strlen(err));
    }
    sqlite3_stmt *stmt = NULL;
    /* tbl_appbrowse_2_appinfo holds title_id + appId + name. Schema
     * stable across PS5 firmware revisions per psdevwiki. */
    const char *sql =
        "SELECT titleId, appId, appName FROM tbl_appbrowse_2_appinfo "
        "WHERE titleId IS NOT NULL ORDER BY titleId";
    rc = sq_prepare(db, sql, -1, &stmt, NULL);
    if (rc != 0 || !stmt) {
        if (stmt) sq_finalize(stmt);
        sq_close(db);
        const char *err = "{\"err\":\"prepare_failed\",\"apps\":[]}";
        return send_frame(client_fd, FTX2_FRAME_APPDB_QUERY_ACK, 0,
                          trace_id, err, strlen(err));
    }
    char *resp = malloc(64 * 1024);
    if (!resp) {
        sq_finalize(stmt);
        sq_close(db);
        const char *err = "{\"err\":\"oom\",\"apps\":[]}";
        return send_frame(client_fd, FTX2_FRAME_APPDB_QUERY_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"apps\":[");
    int wrote_one = 0;
    while ((rc = sq_step(stmt)) == ADB_SQLITE_ROW) {
        const unsigned char *tid = sq_text(stmt, 0);
        int aid = sq_int(stmt, 1);
        const unsigned char *name = sq_text(stmt, 2);
        if (!tid) continue;
        char tid_esc[64];
        char name_esc[512];
        json_escape_into((const char *)tid, tid_esc, sizeof(tid_esc));
        json_escape_into(name ? (const char *)name : "", name_esc, sizeof(name_esc));
        if (n >= cap - 700) break;
        if (wrote_one) resp[n++] = ',';
        wrote_one = 1;
        n += snprintf(resp + n, cap - n,
                      "{\"title_id\":\"%s\",\"app_id\":%d,\"name\":\"%s\"}",
                      tid_esc, aid, name_esc);
    }
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    sq_finalize(stmt);
    sq_close(db);
    int rc2 = send_frame(client_fd, FTX2_FRAME_APPDB_QUERY_ACK, 0,
                         trace_id, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc2;
}

/* ── Direct .pkg mount + UFS fsck ────────────────────────────────────── */

/* Sony's libSceFsInternalForVsh exports — undocumented signatures.
 * Sigs derived from reverse-engineering work and SDK header leaks
 * across multiple firmware revisions. Both are best-effort: we
 * forward the user's args and surface the return code.
 *
 * Resolved via dlopen at first use rather than compile-time linkage.
 * libSceFsInternalForVsh is a VSH-internal SPRX that's blocked from
 * user-mode loaders on some firmware. Compile-time linkage caused
 * rtld to refuse the entire payload (no toast, no port bind, loader
 * silently rejects). With dlopen the handlers degrade gracefully
 * to "service_unavailable" and the rest of the payload works. */
typedef int (*sce_fs_mount_game_pkg_fn)(const char *pkg_path,
                                         const char *mount_point, int flags);
typedef int (*sce_fs_ufs_fsck_fn)(const char *device, int flags, void *opts);
typedef int (*sce_fs_mount_lwfs_fn)(const char *patch_path,
                                     const char *mount_point,
                                     const char *title_id, int flags);
static sce_fs_mount_game_pkg_fn p_sceFsMountGamePkg = NULL;
static sce_fs_ufs_fsck_fn      p_sceFsUfsFsck      = NULL;
static sce_fs_mount_lwfs_fn    p_sceFsMountLwfs    = NULL;
static int sce_fs_internal_resolve_attempted = 0;
static int resolve_sce_fs_internal(void) {
    if (sce_fs_internal_resolve_attempted) {
        /* If at least one symbol resolved we say "ok" — individual
         * call sites null-check their specific function pointer. */
        return (p_sceFsMountGamePkg || p_sceFsUfsFsck || p_sceFsMountLwfs)
            ? 0 : -1;
    }
    sce_fs_internal_resolve_attempted = 1;
    void *h = dlopen("libSceFsInternalForVsh.sprx", RTLD_LAZY);
    if (!h) return -1;
    p_sceFsMountGamePkg = (sce_fs_mount_game_pkg_fn)
        dlsym(h, "sceFsMountGamePkg");
    p_sceFsUfsFsck = (sce_fs_ufs_fsck_fn)
        dlsym(h, "sceFsUfsFsck");
    p_sceFsMountLwfs = (sce_fs_mount_lwfs_fn)
        dlsym(h, "sceFsMountLwfs");
    return (p_sceFsMountGamePkg || p_sceFsUfsFsck || p_sceFsMountLwfs)
        ? 0 : -1;
}

/* Helper: extract a quoted string field from the input JSON body
 * into a fixed buffer. Same one-pass parser as the other handlers
 * use; bounded, no allocation. */
static int parse_json_string_field_local(const char *body, uint64_t body_len,
                                          const char *field, char *out,
                                          size_t out_size) {
    if (!body || body_len == 0 || !field || !out || out_size == 0) return -1;
    char needle[64];
    snprintf(needle, sizeof(needle), "\"%s\"", field);
    const char *body_end = body + body_len;
    const char *p = find_bounded(body, (size_t)body_len, needle);
    if (!p) return -1;
    p += strlen(needle);
    while (p < body_end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
    if (p >= body_end || *p != ':') return -1;
    p++;
    while (p < body_end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
    if (p >= body_end || *p != '"') return -1;
    p++;
    const char *e = json_string_end(p, body_end);
    if (!e) return -1;
    return json_copy_unescaped_string(p, e, out, out_size);
}

static int handle_pkg_direct_mount(runtime_state_t *state, int client_fd,
                                    uint64_t trace_id, const char *body,
                                    uint64_t body_len) {
    if (!state) return -1;
    char pkg_path[512] = {0};
    char mount_point[256] = {0};
    if (parse_json_string_field_local(body, body_len, "pkg_path",
                                       pkg_path, sizeof(pkg_path)) != 0 ||
        pkg_path[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"pkg_path_required\"}";
        return send_frame(client_fd, FTX2_FRAME_PKG_DIRECT_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (parse_json_string_field_local(body, body_len, "mount_point",
                                       mount_point, sizeof(mount_point)) != 0 ||
        mount_point[0] == '\0') {
        /* Default to /mnt/ps5upload/<basename>. */
        const char *base = strrchr(pkg_path, '/');
        base = base ? base + 1 : pkg_path;
        snprintf(mount_point, sizeof(mount_point),
                 "/mnt/ps5upload/%s.mount", base);
    }
    /* Restrict the mount point to writable roots. Without this a
     * client could mount over /system_data, /user/system, etc. — Sony
     * may or may not refuse, and we shouldn't gamble. The pkg source
     * itself is also gated; a hostile pkg lookup outside the writable
     * roots is rejected. */
    if (!is_path_allowed(mount_point) || !is_path_allowed(pkg_path)) {
        const char *err = "{\"ok\":false,\"err\":\"path_not_allowed\"}";
        return send_frame(client_fd, FTX2_FRAME_PKG_DIRECT_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Resolve the optional libSceFsInternalForVsh export at first use. */
    if (resolve_sce_fs_internal() != 0 || !p_sceFsMountGamePkg) {
        const char *err = "{\"ok\":false,\"err\":\"libSceFsInternalForVsh_unavailable\"}";
        return send_frame(client_fd, FTX2_FRAME_PKG_DIRECT_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Best-effort mkdir of the mount point. */
    int created_mp = (mkdir(mount_point, 0755) == 0);
    int rc = p_sceFsMountGamePkg(pkg_path, mount_point, 0);
    if (rc != 0 && created_mp) {
        /* Clean up the empty mount-point dir we just made so a
         * failed attempt doesn't leave litter on the FS. */
        (void)rmdir(mount_point);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char mount_point_esc[512];
    char resp[700];
    json_escape_into(mount_point, mount_point_esc, sizeof(mount_point_esc));
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"code\":%d,\"mount_point\":\"%s\"}",
                     rc == 0 ? "true" : "false", rc, mount_point_esc);
    return send_frame(client_fd, FTX2_FRAME_PKG_DIRECT_MOUNT_ACK, 0,
                      trace_id, resp, (uint64_t)n);
}

static int handle_ufs_fsck(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *body,
                            uint64_t body_len) {
    if (!state) return -1;
    char device[256] = {0};
    if (parse_json_string_field_local(body, body_len, "device",
                                       device, sizeof(device)) != 0 ||
        device[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"device_required\"}";
        return send_frame(client_fd, FTX2_FRAME_UFS_FSCK_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Restrict the device to ones we created (md*, lvd*) plus the
     * external storage devices the PS5 exposes. Without this a
     * client could pass /dev/da0 (system internal disk) with
     * repair=true and corrupt the OS partition. The check is exact-
     * prefix; further numbers/digits are allowed for unit ids. */
    {
        const char *d = device;
        const char *suffix = NULL;
        int allowed = 0;
        if (strncmp(d, "/dev/md", 7) == 0) suffix = d + 7;
        else if (strncmp(d, "/dev/lvd", 8) == 0) suffix = d + 8;
        else if (strncmp(d, "/dev/da", 7) == 0) suffix = d + 7;
        if (suffix && suffix[0] >= '0' && suffix[0] <= '9') {
            const char *s = suffix;
            allowed = 1;
            while (*s) {
                int is_digit = *s >= '0' && *s <= '9';
                int is_lower = *s >= 'a' && *s <= 'z';
                int is_upper = *s >= 'A' && *s <= 'Z';
                if (!is_digit && !is_lower && !is_upper) {
                    allowed = 0;
                    break;
                }
                s++;
            }
        }
        if (strncmp(d, "/dev/da0", 8) == 0) allowed = 0;
        if (!allowed) {
            const char *err = "{\"ok\":false,\"err\":\"device_not_allowed\"}";
            return send_frame(client_fd, FTX2_FRAME_UFS_FSCK_ACK, 0,
                              trace_id, err, strlen(err));
        }
    }
    /* Repair flag — we look for `"repair":true`; anything else is
     * read-only (the safer default). */
    int repair = 0;
    if (body && body_len > 0 && body_len < 1024) {
        char tmp[1028];
        memcpy(tmp, body, (size_t)body_len);
        tmp[body_len] = '\0';
        if (strstr(tmp, "\"repair\":true")) {
            repair = 1;
        }
    }
    if (resolve_sce_fs_internal() != 0 || !p_sceFsUfsFsck) {
        const char *err = "{\"ok\":false,\"err\":\"libSceFsInternalForVsh_unavailable\"}";
        return send_frame(client_fd, FTX2_FRAME_UFS_FSCK_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Sony's flags layout is opaque; per psdevwiki, flag 1 enables
     * write-mode repair and flag 0 is read-only check. The opts
     * pointer is documented as "implementation-specific" — we pass
     * NULL which works for the simple checks we need. */
    int rc = p_sceFsUfsFsck(device, repair ? 1 : 0, NULL);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char device_esc[512];
    char resp[700];
    json_escape_into(device, device_esc, sizeof(device_esc));
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"code\":%d,\"device\":\"%s\",\"repair\":%s}",
                     rc == 0 ? "true" : "false", rc, device_esc,
                     repair ? "true" : "false");
    return send_frame(client_fd, FTX2_FRAME_UFS_FSCK_ACK, 0,
                      trace_id, resp, (uint64_t)n);
}

static int handle_lwfs_mount(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    char patch_path[512] = {0};
    char mount_point[256] = {0};
    char title_id[64] = {0};
    if (parse_json_string_field_local(body, body_len, "patch_path",
                                       patch_path, sizeof(patch_path)) != 0 ||
        patch_path[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"patch_path_required\"}";
        return send_frame(client_fd, FTX2_FRAME_LWFS_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (parse_json_string_field_local(body, body_len, "mount_point",
                                       mount_point, sizeof(mount_point)) != 0 ||
        mount_point[0] == '\0') {
        const char *base = strrchr(patch_path, '/');
        base = base ? base + 1 : patch_path;
        snprintf(mount_point, sizeof(mount_point),
                 "/mnt/ps5upload/%s.lwfs", base);
    }
    parse_json_string_field_local(body, body_len, "title_id",
                                   title_id, sizeof(title_id));
    if (!is_path_allowed(mount_point) || !is_path_allowed(patch_path)) {
        const char *err = "{\"ok\":false,\"err\":\"path_not_allowed\"}";
        return send_frame(client_fd, FTX2_FRAME_LWFS_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (resolve_sce_fs_internal() != 0 || !p_sceFsMountLwfs) {
        const char *err = "{\"ok\":false,\"err\":\"libSceFsInternalForVsh_unavailable\"}";
        return send_frame(client_fd, FTX2_FRAME_LWFS_MOUNT_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int created_mp = (mkdir(mount_point, 0755) == 0);
    int rc = p_sceFsMountLwfs(patch_path, mount_point,
                              title_id[0] ? title_id : NULL, 0);
    if (rc != 0 && created_mp) {
        (void)rmdir(mount_point);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char mount_point_esc[512];
    char title_id_esc[128];
    char resp[800];
    json_escape_into(mount_point, mount_point_esc, sizeof(mount_point_esc));
    json_escape_into(title_id, title_id_esc, sizeof(title_id_esc));
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"code\":%d,\"mount_point\":\"%s\","
                     "\"title_id\":\"%s\"}",
                     rc == 0 ? "true" : "false", rc, mount_point_esc,
                     title_id_esc);
    return send_frame(client_fd, FTX2_FRAME_LWFS_MOUNT_ACK, 0,
                      trace_id, resp, (uint64_t)n);
}

/* ── Atomic small-file write ─────────────────────────────────────────── */

/* Decode base64 into a freshly-malloc'd buffer. Returns NULL on
 * malformed input or oversize. Cap is enforced by the caller. */
static unsigned char *base64_decode(const char *s, size_t s_len, size_t *out_len) {
    static const signed char tab[256] = {
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,62,-1,-1,-1,63,
        52,53,54,55,56,57,58,59,60,61,-1,-1,-1,-1,-1,-1,
        -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12,13,14,
        15,16,17,18,19,20,21,22,23,24,25,-1,-1,-1,-1,-1,
        -1,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,
        41,42,43,44,45,46,47,48,49,50,51,-1,-1,-1,-1,-1,
    };
    size_t out_cap = (s_len / 4) * 3 + 4;
    unsigned char *out = malloc(out_cap);
    if (!out) return NULL;
    int v = 0;
    int bits = 0;
    size_t out_n = 0;
    for (size_t i = 0; i < s_len; i++) {
        unsigned char c = (unsigned char)s[i];
        if (c == '=' || c == '\n' || c == '\r' || c == ' ') continue;
        signed char d = tab[c];
        if (d < 0) {
            free(out);
            return NULL;
        }
        v = (v << 6) | d;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            if (out_n >= out_cap) {
                free(out);
                return NULL;
            }
            out[out_n++] = (unsigned char)((v >> bits) & 0xff);
        }
    }
    *out_len = out_n;
    return out;
}

/* Path safety: must be absolute, no `..` segments, no `://` scheme. */
/* Same allowlist as the rest of the destructive FS handlers. The
 * old standalone implementation accepted "any absolute path without
 * .. or ://" — which let a client write under /system, /system_data,
 * /dev, etc. Per audit, replaced with the central is_path_allowed
 * for consistency. Kept the function for call-site stability. */
static int is_safe_write_path(const char *p) {
    return is_path_allowed(p);
}

#define FS_WRITE_BYTES_MAX 262144  /* 256 KB cap */

static int handle_fs_write_bytes(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *body,
                                  uint64_t body_len) {
    if (!state) return -1;
    if (!body || body_len == 0 || body_len > 512 * 1024) {
        const char *err = "{\"ok\":false,\"err\":\"body_required\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Parse path + base64. We do single-pass extraction; no full JSON
     * library here. */
    char path[1024] = {0};
    char mode[16] = "overwrite";
    if (parse_json_string_field_local(body, body_len, "path",
                                       path, sizeof(path)) != 0 ||
        path[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"path_required\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (!is_safe_write_path(path)) {
        const char *err = "{\"ok\":false,\"err\":\"path_unsafe\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    parse_json_string_field_local(body, body_len, "mode",
                                   mode, sizeof(mode));
    /* Locate the bytes field's value. We need its raw start/end so we
     * don't allocate a huge intermediate buffer to copy it. */
    const char *p = strstr(body, "\"bytes\"");
    if (!p) {
        const char *err = "{\"ok\":false,\"err\":\"bytes_required\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    p = (const char *)memchr(p, ':', body_len - (size_t)(p - body));
    if (!p) {
        const char *err = "{\"ok\":false,\"err\":\"bytes_malformed\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    p++;
    while ((size_t)(p - body) < body_len && (*p == ' ' || *p == '\t')) p++;
    if ((size_t)(p - body) >= body_len || *p != '"') {
        const char *err = "{\"ok\":false,\"err\":\"bytes_malformed\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    p++;  /* past opening quote */
    /* Find closing quote. */
    const char *e = p;
    while ((size_t)(e - body) < body_len && *e != '"') e++;
    if ((size_t)(e - body) >= body_len) {
        const char *err = "{\"ok\":false,\"err\":\"bytes_unterminated\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    size_t b64_len = (size_t)(e - p);
    /* Sanity: 256 KB raw → ~342 KB base64. Cap input length so we
     * don't allocate huge transient buffers for malicious clients. */
    if (b64_len > 380 * 1024) {
        const char *err = "{\"ok\":false,\"err\":\"too_large\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    size_t decoded_len = 0;
    unsigned char *decoded = base64_decode(p, b64_len, &decoded_len);
    if (!decoded) {
        const char *err = "{\"ok\":false,\"err\":\"bad_base64\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    if (decoded_len > FS_WRITE_BYTES_MAX) {
        free(decoded);
        const char *err = "{\"ok\":false,\"err\":\"size_capped\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* `mode=create` rejects pre-existing files. Use stat() to check. */
    if (strcmp(mode, "create") == 0) {
        struct stat st;
        if (stat(path, &st) == 0) {
            free(decoded);
            const char *err = "{\"ok\":false,\"err\":\"exists\"}";
            return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                              trace_id, err, strlen(err));
        }
    }
    /* Atomic write: tmp file → rename. */
    char tmpath[1100];
    snprintf(tmpath, sizeof(tmpath), "%s.ps5upload.tmp", path);
    int fd = open(tmpath, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        free(decoded);
        const char *err = "{\"ok\":false,\"err\":\"open_failed\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    ssize_t written_total = 0;
    while ((size_t)written_total < decoded_len) {
        ssize_t w = write(fd, decoded + written_total,
                          decoded_len - (size_t)written_total);
        if (w <= 0) {
            close(fd);
            unlink(tmpath);
            free(decoded);
            const char *err = "{\"ok\":false,\"err\":\"write_failed\"}";
            return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                              trace_id, err, strlen(err));
        }
        written_total += w;
    }
    fsync(fd);
    close(fd);
    free(decoded);
    if (rename(tmpath, path) != 0) {
        unlink(tmpath);
        const char *err = "{\"ok\":false,\"err\":\"rename_failed\"}";
        return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[128];
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":true,\"size\":%zd}", written_total);
    return send_frame(client_fd, FTX2_FRAME_FS_WRITE_BYTES_ACK, 0,
                      trace_id, resp, (uint64_t)n);
}

/* ── Network round-trip ack ──────────────────────────────────────────── */

/* The "speed test" is observed entirely on the client side. The
 * client sends N empty-body NetSpeedTest frames; the payload just
 * acks each one cheaply. The client measures wall time around the
 * batch and per-frame round-trips. No state on the payload. */
static int handle_net_speed_test(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *body,
                                  uint64_t body_len) {
    if (!state) return -1;
    (void)body;
    (void)body_len;
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *resp = "{\"ok\":true}";
    return send_frame(client_fd, FTX2_FRAME_NET_SPEED_TEST_ACK, 0,
                      trace_id, resp, strlen(resp));
}

/* ── Module enumeration ──────────────────────────────────────────────── */

static int handle_proc_modules(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *body,
                                uint64_t body_len) {
    if (!state) return -1;
    /* `pid` parsed for future use — current sceKernelGetModuleList
     * returns the calling process's modules; we don't have a clean
     * cross-process module enumeration without ptrace. The pid arg
     * is accepted so the protocol remains stable when we add it. */
    (void)body;
    (void)body_len;
    resolve_sce_kernel_extras();
    int handles[256];
    int count = 0;
    int rc = p_sceKernelGetModuleList
                ? p_sceKernelGetModuleList(handles, 256, &count) : -1;
    if (rc != 0) count = 0;
    if (count > 256) count = 256;
    char *resp = malloc(64 * 1024);
    if (!resp) {
        const char *err = "{\"err\":\"oom\",\"modules\":[]}";
        return send_frame(client_fd, FTX2_FRAME_PROC_MODULES_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"modules\":[");
    int wrote_one = 0;
    for (int i = 0; i < count; i++) {
        sce_module_info_t info;
        memset(&info, 0, sizeof(info));
        info.size = sizeof(info);
        if (!p_sceKernelGetModuleInfo) continue;
        if (p_sceKernelGetModuleInfo(handles[i], &info) != 0) continue;
        char esc_name[260];
        json_escape_into(info.name, esc_name, sizeof(esc_name));
        if (n >= cap - 200) break;
        if (wrote_one) resp[n++] = ',';
        wrote_one = 1;
        n += snprintf(resp + n, cap - n,
                      "{\"handle\":%d,\"name\":\"%s\","
                      "\"base\":\"%p\",\"code_size\":%zu}",
                      handles[i], esc_name, info.base_addr, info.code_size);
    }
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc2 = send_frame(client_fd, FTX2_FRAME_PROC_MODULES_ACK, 0,
                         trace_id, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc2;
}

/* ── Rich JSON toast (sceNotificationSend) ───────────────────────────── */

static int handle_toast_send(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    /* Body is the JSON the renderer wants to forward. We don't try to
     * validate it — Sony's daemon parses the template; an invalid
     * shape just produces a silently-dropped notification. We do
     * size-cap to 4 KB to keep an over-eager renderer from streaming
     * huge bodies. */
    if (!body || body_len == 0 || body_len > 4096) {
        const char *err = "{\"ok\":false,\"err\":\"body_required\"}";
        return send_frame(client_fd, FTX2_FRAME_TOAST_SEND_ACK, 0,
                          trace_id, err, strlen(err));
    }
    /* Need a null-terminated copy for sceNotificationSend. */
    char *json = malloc(body_len + 1);
    if (!json) {
        const char *err = "{\"ok\":false,\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_TOAST_SEND_ACK, 0,
                          trace_id, err, strlen(err));
    }
    memcpy(json, body, body_len);
    json[body_len] = '\0';
    /* `target_user_id = -1` = broadcast to all logged-in users.
     * `flag = 0` (system-default formatting). Return is non-zero on
     * malformed JSON or daemon offline; we surface it for the
     * renderer to log but don't treat as fatal. */
    resolve_sce_notification();
    int rc = p_sceNotificationSend
                ? p_sceNotificationSend(-1, 0, json)
                : -1; /* symbol missing on this FW: toast unavailable */
    free(json);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[64];
    int n = snprintf(resp, sizeof(resp), "{\"ok\":%s,\"code\":%d}",
                     rc == 0 ? "true" : "false", rc);
    return send_frame(client_fd, FTX2_FRAME_TOAST_SEND_ACK, 0, trace_id,
                      resp, (uint64_t)n);
}

static int handle_index_cancel(runtime_state_t *state, int client_fd,
                                uint64_t trace_id) {
    if (!state) return -1;
    pthread_mutex_lock(&g_index_lock);
    g_index_cancel = 1;
    pthread_mutex_unlock(&g_index_lock);
    const char *ok = "{\"cancelled\":true}";
    return send_frame(client_fd, FTX2_FRAME_INDEX_CANCEL_ACK, 0,
                      trace_id, ok, strlen(ok));
}

static int handle_list_screenshots(runtime_state_t *state, int client_fd,
                                    uint64_t trace_id) {
    if (!state) return -1;
    char *resp = malloc(64 * 1024);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_LIST_SCREENSHOTS_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"items\":[");
    int wrote_one = 0;
    /* Walk full-resolution first (originals the user actually wants to
     * download), recording each shot's stem; then thumbnails, skipping
     * any whose original was already listed so each shot appears once.
     * Orphan thumbnails (original deleted, thumbnail lingered) still
     * surface as a fallback. */
    ss_seen_t seen = {0};
    seen.cap = 2048;
    seen.names = malloc((size_t)seen.cap * SS_STEM_MAX);
    if (!seen.names) seen.cap = 0;  /* dedup off, listing still works */
    walk_screenshots("/user/av_contents/photo", 5,
                     resp, &n, cap, &wrote_one, &seen, 0);
    walk_screenshots("/user/av_contents/thumbnails/photo", 5,
                     resp, &n, cap, &wrote_one, &seen, 1);
    free(seen.names);
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc = send_frame(client_fd, FTX2_FRAME_LIST_SCREENSHOTS_ACK, 0,
                        trace_id, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* List gameplay video clips (parallels handle_list_screenshots). No
 * thumbnail tree to dedup — a single flat walk of av_contents/video. */
static int handle_list_videos(runtime_state_t *state, int client_fd,
                              uint64_t trace_id) {
    if (!state) return -1;
    char *resp = malloc(64 * 1024);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return send_frame(client_fd, FTX2_FRAME_LIST_VIDEOS_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"items\":[");
    int wrote_one = 0;
    walk_videos("/user/av_contents/video", 5, resp, &n, cap, &wrote_one);
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc = send_frame(client_fd, FTX2_FRAME_LIST_VIDEOS_ACK, 0,
                        trace_id, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

static int handle_hw_storage(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_storage_get_text,
                              FTX2_FRAME_HW_STORAGE_ACK, "hw_storage_failed");
}

static int handle_hw_drive_sensors(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, drive_sensors_get_json,
                              FTX2_FRAME_HW_DRIVE_SENSORS_ACK, "hw_drive_sensors_failed");
}

/* Parse body as "NN" (ASCII decimal). Accepts an empty-body shortcut
 * meaning "reset to default 65 °C" so future UI can send a zero-body
 * frame as a quick reset. Leading whitespace and a trailing newline
 * are tolerated because some shells (lab CLI, curl) add them. */
static int handle_hw_set_fan_threshold(runtime_state_t *state, int client_fd,
                                        uint64_t trace_id,
                                        const char *body, uint64_t body_len) {
    if (!state) return -1;

    uint8_t threshold = 65;  /* Sony's approximate default. */
    if (body_len > 0 && body_len < 16) {
        char buf[16];
        memcpy(buf, body, (size_t)body_len);
        buf[body_len] = '\0';
        /* atoi is fine here — we only trust it for extracting the
         * numeric portion; hw_fan_set_threshold clamps the result,
         * so a non-numeric payload just degrades to 45 °C. */
        int parsed = atoi(buf);
        if (parsed > 0 && parsed < 255) {
            threshold = (uint8_t)parsed;
        }
    } else if (body_len >= 16) {
        static const char err[] = "body_too_long";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, (uint64_t)(sizeof(err) - 1));
    }

    const char *err_reason = NULL;
    if (hw_fan_set_threshold(threshold, &err_reason) != 0) {
        const char *reason = err_reason ? err_reason : "fan_set_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_HW_SET_FAN_THRESHOLD_ACK, 0, trace_id,
                      NULL, 0);
}

/* APP_LAUNCH_BROWSER: open the PS5 web browser. Implementation uses
 * sceSystemServiceLaunchApp with the known-stable NPXS browser title
 * id. Resolved via the same libSceSystemService handle the launch path
 * already uses. */
extern int   register_browser_launch(void);

static int handle_app_launch_browser(runtime_state_t *state, int client_fd,
                                      uint64_t trace_id) {
    if (!state) return -1;
    if (register_browser_launch() != 0) {
        static const char err[] = "launch_browser_unavailable";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, (uint64_t)(sizeof(err) - 1));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_APP_LAUNCH_BROWSER_ACK, 0,
                      trace_id, NULL, 0);
}

/* PROC_LIST: walk allproc via kernel R/W and return a JSON array of
 * running processes. Read-only — we never touch process state here.
 * The body size is bounded so a system with a corrupt proc list can't
 * blow the buffer; see proc_list.c for the truncation policy. */
static int handle_proc_list(runtime_state_t *state, int client_fd,
                             uint64_t trace_id,
                             const char *request_body,
                             uint64_t body_len) {
    /* 64 KiB holds ~600 entries after JSON overhead; real PS5 process
     * counts sit in the 60–120 range. Generous-but-not-absurd cap. */
    const size_t cap = 64u * 1024u;
    char *buf = NULL;
    size_t written = 0;
    const char *err = NULL;
    int rc;
    (void)request_body;
    (void)body_len;
    if (!state) return -1;
    buf = (char *)malloc(cap);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "proc_list_oom", 13);
    }
    if (proc_list_get_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "proc_list_failed";
        rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                        reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_PROC_LIST_ACK, 0, trace_id,
                    buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* PROCESS_LIST: the detailed process enumerate for the in-app process
 * manager — pid/name/comm/title_id/app_id/memory/threads/kind per process.
 * Same sysctl walk as PROC_LIST but the richer proc_list_get_json_ex body.
 * Read-only; no kernel write, no elevation needed for the enumerate. */
static int handle_process_list(runtime_state_t *state, int client_fd,
                               uint64_t trace_id) {
    /* Detailed entries are ~5x the compact ones; 256 KiB holds the busiest
     * real PS5 (~120 procs) with wide headroom. */
    const size_t cap = 256u * 1024u;
    char *buf = NULL;
    size_t written = 0;
    const char *err = NULL;
    int rc;
    if (!state) return -1;
    buf = (char *)malloc(cap);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "process_list_oom", 16);
    }
    if (proc_list_get_json_ex(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "process_list_failed";
        rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                        reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_PROCESS_LIST_ACK, 0, trace_id,
                    buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* PROCESS_KILL: SIGKILL the pid in the request body ({"pid":N}). proc_kill
 * guards self/kernel/init; the UI is responsible for warning before a
 * "system" kill. Ack {"ok":bool,"pid":N[,"err":"..."]}. */
static int handle_process_kill(runtime_state_t *state, int client_fd,
                               uint64_t trace_id, const char *body) {
    int pid = (int)extract_json_uint64_field(body ? body : "", "pid");
    int rc = proc_kill(pid);
    int err_no = errno; /* capture immediately — proc_kill set it on failure */
    char ack[160];
    int n;
    if (rc == 0) {
        n = snprintf(ack, sizeof(ack), "{\"ok\":true,\"pid\":%d}", pid);
        if (state) {
            pthread_mutex_lock(&state->state_mtx);
            state->command_count += 1;
            pthread_mutex_unlock(&state->state_mtx);
        }
    } else {
        /* Report the specific reason (ESRCH = already gone, EPERM = refused /
         * guarded) so a bug report distinguishes "process vanished" from
         * "kernel said no" instead of a bare kill_failed. strerror is bounded
         * and JSON-safe (ASCII), but escape defensively anyway. */
        char reason_esc[96];
        json_escape_into(strerror(err_no), reason_esc, sizeof(reason_esc));
        n = snprintf(ack, sizeof(ack),
                     "{\"ok\":false,\"pid\":%d,\"err\":\"kill_failed\","
                     "\"errno\":%d,\"reason\":\"%s\"}",
                     pid, err_no, reason_esc);
    }
    if (n <= 0 || n >= (int)sizeof(ack)) {
        const char *e = "{\"ok\":false,\"err\":\"format\"}";
        return send_frame(client_fd, FTX2_FRAME_PROCESS_KILL_ACK, 0,
                          trace_id, e, strlen(e));
    }
    return send_frame(client_fd, FTX2_FRAME_PROCESS_KILL_ACK, 0,
                      trace_id, ack, (size_t)n);
}

/* SYSLOG_TAIL: return the PS5 kernel-log circular buffer (dmesg
 * equivalent). Read via `sysctl kern.msgbuf` — the kernel's in-memory
 * printk/printf history. Used by the desktop's "PS5 system log" panel to
 * surface "why did the payload not load / why is X silently failing"
 * without making the user FTP / ssh in. 64 KiB cap is well above the
 * default PS5 msgbuf size; if the kernel was rebuilt with a smaller
 * buffer the sysctl just returns less and we ack the actual length.
 */
static int handle_syslog_tail(runtime_state_t *state, int client_fd,
                              uint64_t trace_id) {
    /* Hard cap so a freshly-rebuilt kernel with an absurd msgbuf size
     * (or a sysctl that reports something pathological) can't OOM us. */
    const size_t HARD_CAP = 1024u * 1024u;
    char *buf = NULL;
    size_t needed = 0;
    int rc;
    if (!state) return -1;
    /* PS5's kernel msgbuf can be 64-256 KiB depending on firmware build,
     * easily larger than the per-syslog-call hardcoded 64 KiB we used
     * to allocate (which fails with ENOMEM=12). Two-pass: first call
     * with NULL buffer to learn the real size, then malloc + read. */
    if (sysctlbyname("kern.msgbuf", NULL, &needed, NULL, 0) != 0) {
        int saved = errno;
        char reason[64];
        int rn = snprintf(reason, sizeof(reason),
                          "syslog_tail_sysctl_size_errno_%d", saved);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, rn > 0 ? (uint64_t)rn : 0);
    }
    if (needed == 0) {
        /* Empty msgbuf is a successful read of zero bytes. */
        return send_frame(client_fd, FTX2_FRAME_SYSLOG_TAIL_ACK, 0,
                          trace_id, "", 0);
    }
    /* Cap the *allocation*, but pass the original `needed` to sysctl so
     * the kernel knows we'd accept up to the real size. FreeBSD's sysctl
     * truncates the copy to whatever buf size we declare via the in/out
     * `sz` arg — but it also returns ENOMEM when the destination is
     * smaller than the data unless we tell it "yes, please truncate."
     * Solution: allocate min(needed, HARD_CAP), tell sysctl `sz =
     * allocated`, and accept truncation. Also retry once on a transient
     * ENOMEM (msgbuf grew between the size-probe and the read — the
     * kernel printk ring is live), bumping our buffer up to HARD_CAP
     * before giving up. */
    size_t alloc = needed > HARD_CAP ? HARD_CAP : needed;
    buf = (char *)malloc(alloc);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "syslog_tail_oom", 15);
    }
    size_t sz = alloc;
    int rcv = sysctlbyname("kern.msgbuf", buf, &sz, NULL, 0);
    if (rcv != 0 && errno == ENOMEM && alloc < HARD_CAP) {
        /* Grew between calls (or initial size-probe under-reported on
         * some FreeBSD versions). One retry at HARD_CAP — if that's
         * still too small, the user just sees a partial tail with the
         * newest entries, which is what dmesg(8) does anyway. */
        free(buf);
        alloc = HARD_CAP;
        buf = (char *)malloc(alloc);
        if (!buf) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "syslog_tail_oom", 15);
        }
        sz = alloc;
        rcv = sysctlbyname("kern.msgbuf", buf, &sz, NULL, 0);
    }
    if (rcv != 0) {
        int saved = errno;
        char reason[64];
        int rn = snprintf(reason, sizeof(reason),
                          "syslog_tail_sysctl_errno_%d", saved);
        free(buf);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, rn > 0 ? (uint64_t)rn : 0);
    }
    /* sz holds the actual byte count written by sysctl. The buffer is
     * plain text (kernel printf output) — let the client treat it as a
     * sized byte slice (no NUL-termination assumption). */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_SYSLOG_TAIL_ACK, 0, trace_id,
                    buf, (uint64_t)sz);
    free(buf);
    return rc;
}

/* ── PKG_INSTALL handlers ─────────────────────────────────────────────────────
 *
 * The host sends PKG_INSTALL with a JSON body carrying the `.pkg` URL it
 * wants Sony's BGFT service to fetch + install. We extract the fields,
 * call into bgft.c which loads libSceBgft.sprx and registers/starts a
 * BGFT task, and respond with the task_id.
 *
 * Sony's installer runs asynchronously inside PS5 firmware; status is
 * polled via PKG_INSTALL_STATUS which calls bgft_install_status() to
 * read BGFT's progress struct.
 */

#include "bgft.h"

static int handle_pkg_install(runtime_state_t *state, int client_fd,
                               uint64_t trace_id,
                               const char *body, uint64_t body_len) {
    char url[1024];
    char content_id[64];
    char title[256];
    char package_type[16];
    char method[32];
    uint64_t size = 0;
    int32_t task_id = -1;
    uint32_t err_code = 0;
    /* 1 KiB ack buffer — pre-2.2.52 was 256, which silently truncated
     * once we added the 2.2.52 diagnostics (`register_path`, `intdebug_avail`,
     * `kernel_rw`) on top of the existing detail string. The detail
     * string can hit ~200 bytes from sceAppInstUtil error decode, plus
     * the new fields plus the fixed-format envelope. 1 KiB has comfortable
     * headroom. */
    char ack[1024];
    int n;
    if (!state || !body) {
        const char *e = "pkg_install_invalid";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)strlen(e));
    }
    /* Hard cap on body length — defensive against malformed input.
     * Using `>=` (not `>`) so we leave room for the NUL terminator
     * we write at `json_buf[body_len]` below. With `>` and an
     * exactly-sized 16384-byte body, the NUL would land at index
     * 16384 — one past the end of the 16384-byte stack buffer. The
     * pre-fix-round-3 form silently corrupted the next stack slot
     * on max-sized bodies. */
    char json_buf[16384];
    if (body_len >= sizeof(json_buf)) {
        const char *e = "pkg_install_body_too_large";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)strlen(e));
    }
    /* The body is JSON but extract_json_string_field expects a
     * NUL-terminated buffer — we copy into a local buffer and
     * NUL-terminate. */
    memcpy(json_buf, body, (size_t)body_len);
    json_buf[body_len] = '\0';

    extract_json_string_field(json_buf, "url", url, sizeof(url));
    extract_json_string_field(json_buf, "content_id", content_id, sizeof(content_id));
    extract_json_string_field(json_buf, "title", title, sizeof(title));
    extract_json_string_field(json_buf, "package_type", package_type, sizeof(package_type));
    /* Optional single-tier selector (engine-driven cascade). Absent ⇒ ""
     * ⇒ payload's legacy internal cascade. */
    method[0] = '\0';
    extract_json_string_field(json_buf, "method", method, sizeof(method));
    size = extract_json_uint64_field(json_buf, "size");

    if (url[0] == '\0') {
        const char *e = "pkg_install_url_missing";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)strlen(e));
    }
    if (content_id[0] == '\0') {
        /* Some non-standard PKGs have no content_id; pass an empty
         * string and let BGFT decide whether to accept. */
    }
    if (package_type[0] == '\0') {
        snprintf(package_type, sizeof(package_type), "PS4GD");
    }

    int rc = bgft_install_start(url, content_id, size, title, package_type,
                                 method, &task_id, &err_code);
    /* Always reply with PKG_INSTALL_ACK carrying the err_code (even on
     * failure) so the host can map it to a user-facing message. We
     * only emit ERROR for true protocol-level violations (bad body). */
    const char *detail = "";
    if (rc != 0) {
        const char *r = bgft_install_unavailable_reason();
        if (r) detail = r;
    }
    /* Diagnostic fields surfaced to the host so a failed install can
     * be diagnosed without ssh:
     *   register_path  — which Register variant we used / would use
     *                    ("intdebug" / "regular" / "none")
     *   intdebug_avail — whether IntDebug Register symbol resolved
     *                    (0/1; if 0 fakepkgs are likely to fail with
     *                     entitlement errors regardless of cred state)
     *   kernel_rw      — whether process-wide ucred elevation succeeded
     *                    (mirrors STATUS_ACK's `ucred_elevated` field).
     *                    g_ucred_elevation_rc lives in main.c. */
    extern volatile int g_ucred_elevation_rc;
    const char *register_path = bgft_install_last_register_path();
    int intdebug_avail = bgft_install_intdebug_available();
    int kernel_rw = (g_ucred_elevation_rc == 0) ? 1 : 0;
    /* 2.2.52-fix: per-tier error codes. UINT32_MAX = tier not
     * attempted; surface as JSON null so the host UI can show "—"
     * rather than a misleading huge number. */
    uint32_t shellui_err = bgft_install_last_shellui_err();
    uint32_t appinst_err = bgft_install_last_appinst_err();

    /* Manual JSON encode — small + deterministic, no need for a JSON
     * library on the payload side. Escape the detail string defensively:
     * `bgft_install_unavailable_reason()` returns SDK-supplied text
     * which could in principle contain a `"` or `\`, breaking the
     * host-side decoder. */
    char detail_esc[512];
    char path_esc[32];
    json_escape_into(detail, detail_esc, sizeof(detail_esc));
    json_escape_into(register_path, path_esc, sizeof(path_esc));
    /* Emit `intdebug_avail` / `kernel_rw` as JSON booleans (`true` /
     * `false`), not as integer 0/1. The engine's PkgInstallResponse
     * declares both fields as `bool` and serde's default decoder
     * rejects integer-for-bool with `invalid type: integer 1, expected
     * a boolean` — that single error rejected the entire ACK decode,
     * so a 2.2.52 payload with the `%d` form broke every pkg install
     * (not just the new diagnostics). Stringify to JSON booleans so
     * decode succeeds. */
    const char *intdebug_str = intdebug_avail ? "true" : "false";
    const char *kernel_rw_str = kernel_rw ? "true" : "false";
    char shellui_buf[24];
    char appinst_buf[24];
    /* 2.2.54-fix-round-8: emit null only when the tier wasn't
     * attempted, NOT when its err code happens to be 0xFFFFFFFF.
     * Pre-fix used UINT32_MAX as the sentinel — but that collided
     * with pt_call returning -1 (Sony err code = 0xFFFFFFFF), making
     * legitimate failures appear as "tier never ran" in diag. */
    if (bgft_install_last_shellui_err_set()) {
        snprintf(shellui_buf, sizeof(shellui_buf), "%u", (unsigned)shellui_err);
    } else {
        snprintf(shellui_buf, sizeof(shellui_buf), "null");
    }
    if (bgft_install_last_appinst_err_set()) {
        snprintf(appinst_buf, sizeof(appinst_buf), "%u", (unsigned)appinst_err);
    } else {
        snprintf(appinst_buf, sizeof(appinst_buf), "null");
    }
    n = snprintf(ack, sizeof(ack),
                 "{\"task_id\":%d,\"err_code\":%u,\"detail\":\"%s\","
                 "\"register_path\":\"%s\",\"intdebug_avail\":%s,"
                 "\"kernel_rw\":%s,\"shellui_err\":%s,\"appinst_err\":%s}",
                 task_id, (unsigned)err_code, detail_esc,
                 path_esc, intdebug_str, kernel_rw_str,
                 shellui_buf, appinst_buf);
    if (n < 0 || n >= (int)sizeof(ack)) {
        /* Truncated — drop the variable-length detail but keep the
         * fixed-size diagnostics so the host still sees the error
         * code + register path. The verbose detail string lives in
         * stderr/klog regardless. */
        n = snprintf(ack, sizeof(ack),
                     "{\"task_id\":%d,\"err_code\":%u,\"detail\":\"\","
                     "\"register_path\":\"%s\",\"intdebug_avail\":%s,"
                     "\"kernel_rw\":%s,\"shellui_err\":%s,\"appinst_err\":%s}",
                     task_id, (unsigned)err_code,
                     path_esc, intdebug_str, kernel_rw_str,
                     shellui_buf, appinst_buf);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_PKG_INSTALL_ACK, 0, trace_id,
                      ack, (uint64_t)n);
}

static int handle_pkg_install_status(runtime_state_t *state, int client_fd,
                                      uint64_t trace_id,
                                      const char *body, uint64_t body_len) {
    bgft_phase_t phase = BGFT_PHASE_QUEUED;
    uint64_t downloaded = 0;
    uint64_t total = 0;
    uint32_t err_code = 0;
    /* 384 B — pre-fix-round-2 was 256 which silently truncated once
     * the diagnostic suffix (`register_path` / `intdebug_avail` /
     * `kernel_rw`) was added. The status frame is polled at 1 Hz
     * during an active install, so a buffer that's just-barely-big-
     * enough makes the truncation fallback (which drops downloaded /
     * total) the worst-case rendering — bumping gives comfortable
     * headroom without enlarging the wire shape for normal traffic. */
    char ack[384];
    int n;
    if (!state || !body) {
        const char *e = "pkg_install_status_invalid";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)strlen(e));
    }
    char json_buf[256];
    if (body_len >= sizeof(json_buf)) {
        /* Same off-by-one fix as handle_pkg_install: `>=` reserves
         * the slot at `json_buf[body_len]` for the NUL terminator. */
        const char *e = "pkg_install_status_body_too_large";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)strlen(e));
    }
    memcpy(json_buf, body, (size_t)body_len);
    json_buf[body_len] = '\0';
    int task_id = (int)extract_json_uint64_field(json_buf, "task_id");
    int rc = bgft_install_status(task_id, &phase, &downloaded, &total,
                                  &err_code);
    const char *phase_str = "queued";
    switch (phase) {
        case BGFT_PHASE_QUEUED:   phase_str = "queued"; break;
        case BGFT_PHASE_DOWNLOAD: phase_str = "download"; break;
        case BGFT_PHASE_INSTALL:  phase_str = "install"; break;
        case BGFT_PHASE_DONE:     phase_str = "done"; break;
        case BGFT_PHASE_ERROR:    phase_str = "error"; break;
    }
    /* Even on rc != 0 we still surface what we can — err_code carries
     * the BGFT error code or our sentinel and the host maps it to a
     * message. */
    (void)rc;

    /* Console-toast on the first done/error transition for this
     * task_id. The host polls status at 1 Hz, so without
     * deduplication we'd send a toast every second after BGFT
     * finishes — annoying. A small ring of recently-toasted task_ids
     * (16 slots) covers any realistic install-queue depth without
     * tracking forever. Per-state toast (one for done, one for
     * error) is fine because we check both states on insert. */
    if (phase == BGFT_PHASE_DONE || phase == BGFT_PHASE_ERROR) {
        static pthread_mutex_t toasted_mtx = PTHREAD_MUTEX_INITIALIZER;
        static int toasted_task_ids[16] = {0};
        static int toasted_phase[16] = {0};
        static int toasted_next = 0;
        int target_phase = (int)phase;
        int already = 0;
        pthread_mutex_lock(&toasted_mtx);
        for (int i = 0; i < 16; i++) {
            if (toasted_task_ids[i] == task_id &&
                toasted_phase[i] == target_phase) {
                already = 1;
                break;
            }
        }
        if (!already) {
            toasted_task_ids[toasted_next] = task_id;
            toasted_phase[toasted_next] = target_phase;
            toasted_next = (toasted_next + 1) % 16;
        }
        pthread_mutex_unlock(&toasted_mtx);
        if (!already) {
            char toast[160];
            if (phase == BGFT_PHASE_DONE) {
                snprintf(toast, sizeof(toast),
                         "ps5upload: install complete (task %d)", task_id);
            } else {
                snprintf(toast, sizeof(toast),
                         "ps5upload: install failed (task %d, code 0x%08x)",
                         task_id, (unsigned)err_code);
            }
            pop_notification(toast);
        }
    }

    /* Re-emit the same diagnostics the install/start ACK carries.
     * If BGFT transitions to phase=error mid-install, the user's
     * "Why?" disclosure should reflect the live state — pre-fix the
     * client only saw the diag captured at install/start, which by
     * then had said everything was fine. The three globals are
     * cheap to read (one stat-equivalent + a pointer + one int). */
    extern volatile int g_ucred_elevation_rc;
    const char *register_path = bgft_install_last_register_path();
    int intdebug_avail = bgft_install_intdebug_available();
    int kernel_rw = (g_ucred_elevation_rc == 0) ? 1 : 0;
    uint32_t shellui_err = bgft_install_last_shellui_err();
    uint32_t appinst_err = bgft_install_last_appinst_err();
    char path_esc[32];
    json_escape_into(register_path, path_esc, sizeof(path_esc));
    const char *intdebug_str = intdebug_avail ? "true" : "false";
    const char *kernel_rw_str = kernel_rw ? "true" : "false";
    char shellui_buf[24];
    char appinst_buf[24];
    /* 2.2.54-fix-round-8: emit null only when the tier wasn't
     * attempted, NOT when its err code happens to be 0xFFFFFFFF.
     * Pre-fix used UINT32_MAX as the sentinel — but that collided
     * with pt_call returning -1 (Sony err code = 0xFFFFFFFF), making
     * legitimate failures appear as "tier never ran" in diag. */
    if (bgft_install_last_shellui_err_set()) {
        snprintf(shellui_buf, sizeof(shellui_buf), "%u", (unsigned)shellui_err);
    } else {
        snprintf(shellui_buf, sizeof(shellui_buf), "null");
    }
    if (bgft_install_last_appinst_err_set()) {
        snprintf(appinst_buf, sizeof(appinst_buf), "%u", (unsigned)appinst_err);
    } else {
        snprintf(appinst_buf, sizeof(appinst_buf), "null");
    }
    n = snprintf(ack, sizeof(ack),
                 "{\"phase\":\"%s\",\"downloaded\":%llu,\"total\":%llu,"
                 "\"err_code\":%u,\"detail\":\"\","
                 "\"register_path\":\"%s\",\"intdebug_avail\":%s,"
                 "\"kernel_rw\":%s,\"shellui_err\":%s,\"appinst_err\":%s}",
                 phase_str,
                 (unsigned long long)downloaded,
                 (unsigned long long)total,
                 (unsigned)err_code,
                 path_esc, intdebug_str, kernel_rw_str,
                 shellui_buf, appinst_buf);
    if (n < 0 || n >= (int)sizeof(ack)) {
        n = snprintf(ack, sizeof(ack),
                     "{\"phase\":\"%s\",\"downloaded\":0,\"total\":0,"
                     "\"err_code\":%u,\"detail\":\"\","
                     "\"register_path\":\"%s\",\"intdebug_avail\":%s,"
                     "\"kernel_rw\":%s,\"shellui_err\":%s,\"appinst_err\":%s}",
                     phase_str, (unsigned)err_code,
                     path_esc, intdebug_str, kernel_rw_str,
                     shellui_buf, appinst_buf);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_PKG_INSTALL_STATUS_ACK, 0,
                      trace_id, ack, (uint64_t)n);
}

/* ── Binary frame dispatcher ─────────────────────────────────────────────────── */

/* Frame-type → port classification. Transfer frames carry bulk data and
 * serialize a single client on port 9113; management frames are small,
 * responsive, and live on port 9114 in their own pthread. Keeping the
 * split strict (ERROR+close on a mismatch) prevents a badly-written
 * client from STREAM_SHARDing onto the mgmt port and DoS'ing it. */
/* ── Profile (avatar + offline-account) frame handlers ──────────────────── */

static int handle_profile_info(int client_fd, uint64_t trace_id) {
    /* Serialize the sceRegMgr/sceUserService calls below: these Sony APIs
     * are not safe to call concurrently from multiple connection threads
     * (the desktop's Profile screen calls this while background status
     * polling also hits the payload), and doing so crashes the process.
     * Same lock register.c/bgft.c use for their Sony calls. The network
     * send_frame() happens AFTER unlock so we don't hold the lock across I/O. */
    pthread_mutex_lock(&sony_api_lock);
    sceUserServiceInitialize(NULL); /* idempotent; name lookups need it */
    char username[64] = {0};
    uint32_t uid = profile_foreground_user(username, sizeof(username));
    char uesc[128];
    json_escape_into(username, uesc, sizeof(uesc));

    char body[4096];
    int len = snprintf(body, sizeof(body),
        "{\"ok\":true,\"uid\":%u,\"uid_hex\":\"0x%08X\","
        "\"username\":\"%s\",\"slots\":[",
        uid, uid, uesc);
    if (len < 0 || len >= (int)sizeof(body)) {
        pthread_mutex_unlock(&sony_api_lock);
        const char *err = "{\"ok\":false,\"err\":\"format\"}";
        return send_frame(client_fd, FTX2_FRAME_PROFILE_INFO_ACK, 0,
                          trace_id, err, strlen(err));
    }
    int first = 1;
    for (int s = 1; s <= PROFILE_SLOT_COUNT; s++) {
        char name[PROFILE_NAME_MAX] = {0};
        char type[PROFILE_TYPE_MAX] = {0};
        uint64_t id = 0;
        int flags = 0;
        if (profile_slot_get_name(s, name, NULL) != 0 || !name[0]) continue;
        profile_slot_get_id(s, &id, NULL);
        profile_slot_get_type(s, type, NULL);
        profile_slot_get_flags(s, &flags, NULL);
        char nesc[PROFILE_NAME_MAX * 2 + 2];
        char tesc[PROFILE_TYPE_MAX * 2 + 2];
        json_escape_into(name, nesc, sizeof(nesc));
        json_escape_into(type, tesc, sizeof(tesc));
        int activated = (id != 0 && flags == PROFILE_DEFAULT_FLAGS);
        int n = snprintf(body + len, sizeof(body) - len,
            "%s{\"slot\":%d,\"name\":\"%s\",\"type\":\"%s\",\"flags\":%d,"
            "\"id\":\"0x%016llx\",\"activated\":%s}",
            first ? "" : ",", s, nesc, tesc, flags,
            (unsigned long long)id, activated ? "true" : "false");
        if (n <= 0 || n >= (int)(sizeof(body) - len)) break;
        len += n;
        first = 0;
    }
    /* Close the slots array, then enumerate the console's local users.
     * Primary source: sceUserServiceGetLoginUserIdList — gives the real
     * display name via GetUserName even when there's no foreground user
     * (login != foreground). Supplemented by a /user/home scan for any
     * on-disk user not currently logged in (uid known, name may be empty).
     * The uid is the same value the profile-cache path uses. */
    int mid = snprintf(body + len, sizeof(body) - len, "],\"users\":[");
    if (mid > 0 && mid < (int)(sizeof(body) - len)) len += mid;

    int seen[USER_SERVICE_MAX_USERS];
    int seen_count = 0;
    int ufirst = 1;

    int login_ids[USER_SERVICE_MAX_USERS];
    for (int i = 0; i < USER_SERVICE_MAX_USERS; i++) login_ids[i] = -1;
    sceUserServiceGetLoginUserIdList(login_ids);
    for (int i = 0; i < USER_SERVICE_MAX_USERS; i++) {
        if (login_ids[i] < 0) continue;
        uint32_t uuid = (uint32_t)login_ids[i];
        char uname[64] = {0};
        sceUserServiceGetUserName(login_ids[i], uname, sizeof(uname));
        char uesc2[128];
        json_escape_into(uname, uesc2, sizeof(uesc2));
        int un = snprintf(body + len, sizeof(body) - len,
            "%s{\"uid\":%u,\"uid_hex\":\"0x%08X\",\"username\":\"%s\"}",
            ufirst ? "" : ",", uuid, uuid, uesc2);
        if (un <= 0 || un >= (int)(sizeof(body) - len)) break;
        len += un;
        ufirst = 0;
        if (seen_count < USER_SERVICE_MAX_USERS) seen[seen_count++] = login_ids[i];
    }

    DIR *ud = opendir("/user/home");
    if (ud) {
        struct dirent *ue;
        while ((ue = readdir(ud)) != NULL) {
            /* Accept exactly 8 hex chars (a user id dir). */
            const char *nm = ue->d_name;
            int hexlen = 0;
            while (nm[hexlen]) {
                char hc = nm[hexlen];
                int is_hex = (hc >= '0' && hc <= '9') ||
                             (hc >= 'a' && hc <= 'f') ||
                             (hc >= 'A' && hc <= 'F');
                if (!is_hex) break;
                hexlen++;
            }
            if (hexlen != 8 || nm[8] != '\0') continue;
            uint32_t uuid = (uint32_t)strtoul(nm, NULL, 16);
            int already = 0;
            for (int k = 0; k < seen_count; k++) {
                if ((uint32_t)seen[k] == uuid) {
                    already = 1;
                    break;
                }
            }
            if (already) continue;
            char uname[64] = {0};
            char uesc2[128];
            profile_user_name(uuid, uname, sizeof(uname));
            json_escape_into(uname, uesc2, sizeof(uesc2));
            int un = snprintf(body + len, sizeof(body) - len,
                "%s{\"uid\":%u,\"uid_hex\":\"0x%08X\",\"username\":\"%s\"}",
                ufirst ? "" : ",", uuid, uuid, uesc2);
            if (un <= 0 || un >= (int)(sizeof(body) - len)) break;
            len += un;
            ufirst = 0;
        }
        closedir(ud);
    }
    int tail = snprintf(body + len, sizeof(body) - len, "]}");
    if (tail > 0 && tail < (int)(sizeof(body) - len)) len += tail;
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_INFO_ACK, 0,
                      trace_id, body, (uint64_t)len);
}

static int handle_profile_set_username(int client_fd, uint64_t trace_id,
                                       const char *body) {
    int slot = (int)extract_json_uint64_field(body, "slot");
    char name[PROFILE_NAME_MAX] = {0};
    extract_json_string_field(body, "name", name, sizeof(name));
    uint32_t err = 0;
    int rc = -1;
    if (slot >= 1 && slot <= PROFILE_SLOT_COUNT && name[0]) {
        pthread_mutex_lock(&sony_api_lock);
        rc = profile_slot_set_name(slot, name, &err);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
    }
    char nesc[PROFILE_NAME_MAX * 2 + 2];
    json_escape_into(name, nesc, sizeof(nesc));
    char resp[256];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"slot\":%d,\"name\":\"%s\",\"err_code\":%u}",
        rc == 0 ? "true" : "false", slot, nesc, err);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_SET_USERNAME_ACK, 0,
                      trace_id, resp, (uint64_t)len);
}

static int handle_profile_activate(int client_fd, uint64_t trace_id,
                                   const char *body) {
    int slot = (int)extract_json_uint64_field(body, "slot");
    /* Optional explicit id (hex "0x.." or decimal); 0/absent → derive. */
    char idstr[32] = {0};
    extract_json_string_field(body, "id", idstr, sizeof(idstr));
    uint64_t id = 0;
    if (idstr[0]) {
        if (idstr[0] == '0' && (idstr[1] == 'x' || idstr[1] == 'X')) {
            id = strtoull(idstr + 2, NULL, 16);
        } else {
            id = strtoull(idstr, NULL, 0);
        }
    }
    int rc = -1;
    uint64_t actual = 0;
    if (slot >= 1 && slot <= PROFILE_SLOT_COUNT) {
        pthread_mutex_lock(&sony_api_lock);
        rc = profile_slot_activate(slot, id);
        profile_slot_get_id(slot, &actual, NULL);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
    }
    char resp[160];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"slot\":%d,\"id\":\"0x%016llx\"}",
        rc == 0 ? "true" : "false", slot, (unsigned long long)actual);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_ACTIVATE_ACK, 0,
                      trace_id, resp, (uint64_t)len);
}

static int handle_profile_clear_slot(int client_fd, uint64_t trace_id,
                                     const char *body) {
    int slot = (int)extract_json_uint64_field(body, "slot");
    int rc = -1;
    if (slot >= 1 && slot <= PROFILE_SLOT_COUNT) {
        pthread_mutex_lock(&sony_api_lock);
        rc = profile_slot_clear(slot);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
    }
    char resp[96];
    int len = snprintf(resp, sizeof(resp), "{\"ok\":%s,\"slot\":%d}",
                       rc == 0 ? "true" : "false", slot);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_CLEAR_SLOT_ACK, 0,
                      trace_id, resp, (uint64_t)len);
}

static int handle_profile_apply_avatar(int client_fd, uint64_t trace_id,
                                       const char *body) {
    uint32_t uid = (uint32_t)extract_json_uint64_field(body, "uid");
    if (uid == 0) {
        pthread_mutex_lock(&sony_api_lock);
        uid = profile_foreground_user(NULL, 0);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
    }
    /* profile_apply_avatar is filesystem-only (no Sony APIs), so it runs
     * without the lock — it can be slow (copies 11 files) and needn't
     * block other Sony calls. */
    int copied = 0;
    int rc = (uid != 0) ? profile_apply_avatar(uid, &copied) : -1;
    char resp[160];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"uid\":%u,\"uid_hex\":\"0x%08X\",\"copied\":%d}",
        rc == 0 ? "true" : "false", uid, uid, copied);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_APPLY_AVATAR_ACK, 0,
                      trace_id, resp, (uint64_t)len);
}

/* Keep the home-screen display name in sync after a rename.
 *
 * sceUserServiceSetUserName() updates the live user name (what the app's
 * Profile screen + the PS5 "add profile" uniqueness check read), but the PS5
 * HOME SCREEN displays the np profile-cache `online.json` "firstName" — and
 * SetUserName does NOT touch that file. So once an avatar has been applied
 * (which writes online.json), renaming leaves the home screen showing the OLD
 * name. (HW-confirmed on FW: rename → username changes, online.json firstName
 * stays stale.)
 *
 * Fix: after a successful rename, if the profile cache's online.json EXISTS,
 * rewrite just its "firstName" value in place so the two stores stay
 * consistent. We only touch an EXISTING cache file: a user who never applied
 * an avatar has no cache, the home screen reads the UserService name directly
 * (already updated), and we must NOT create an online.json-only cache (that
 * would blank their avatar). Reading + patching a single field preserves the
 * avatar .dds files and every other online.json field untouched. */
static void profile_sync_online_json_firstname(uint32_t uid, const char *name) {
    char path[256];
    snprintf(path, sizeof(path),
             "/system_data/priv/cache/profile/0x%08X/online.json", uid);

    int fd = open(path, O_RDONLY);
    if (fd < 0) return; /* no cache → nothing to sync (home reads UserService name) */
    char buf[2048];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) return;
    buf[n] = '\0';

    const char *key = "\"firstName\":\"";
    char *p = strstr(buf, key);
    if (!p) return;
    char *vstart = p + strlen(key);
    char *vend = strchr(vstart, '"');
    if (!vend) return;

    char nesc[128];
    json_escape_into(name, nesc, sizeof(nesc)); /* escapes content, no quotes */

    char out[2560];
    size_t prefix_len = (size_t)(vstart - buf);
    int w = snprintf(out, sizeof(out), "%.*s%s%s",
                     (int)prefix_len, buf, nesc, vend);
    if (w < 0 || (size_t)w >= sizeof(out)) return;

    /* Atomic replace: write a sibling tmp then rename over the original so a
     * crash mid-write can't leave a truncated online.json. */
    char tmp[300];
    snprintf(tmp, sizeof(tmp), "%s.ps5up-tmp", path);
    int wf = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (wf < 0) return;
    int wr = write_full(wf, out, (size_t)w);
    close(wf);
    if (wr != 0) {
        (void)unlink(tmp);
        return;
    }
    if (rename(tmp, path) != 0) (void)unlink(tmp);
}

static int handle_profile_set_local_username(int client_fd, uint64_t trace_id,
                                             const char *body) {
    uint32_t uid = (uint32_t)extract_json_uint64_field(body, "uid");
    char name[64] = {0};
    extract_json_string_field(body, "name", name, sizeof(name));
    int rc = -1;
    if (uid != 0 && name[0]) {
        pthread_mutex_lock(&sony_api_lock);
        rc = profile_set_local_username(uid, name);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&sony_api_lock);
        /* Sync the home-screen display name (online.json firstName) — outside
         * sony_api_lock since it's plain file I/O, not a Sony API call. */
        if (rc == 0) profile_sync_online_json_firstname(uid, name);
    }
    char nesc[128];
    json_escape_into(name, nesc, sizeof(nesc));
    char resp[224];
    int len = snprintf(resp, sizeof(resp),
        "{\"ok\":%s,\"uid\":%u,\"uid_hex\":\"0x%08X\",\"name\":\"%s\"}",
        rc == 0 ? "true" : "false", uid, uid, nesc);
    return send_frame(client_fd, FTX2_FRAME_PROFILE_SET_LOCAL_USERNAME_ACK, 0,
                      trace_id, resp, (uint64_t)len);
}

static int is_transfer_frame_type(uint16_t t) {
    return t == FTX2_FRAME_BEGIN_TX
        || t == FTX2_FRAME_STREAM_SHARD
        || t == FTX2_FRAME_COMMIT_TX
        || t == FTX2_FRAME_ABORT_TX;
}

/* `tx_ctx` is NULL on the mgmt port (no transfer frames flow there). On
 * the transfer port, pass a per-connection `conn_tx_ctx_t` so the
 * accept-loop wrapper can mark an unfinished tx as interrupted when the
 * socket closes without seeing COMMIT/ABORT. */
static int handle_binary_frame(runtime_state_t *state, int client_fd,
                               int is_transfer_port,
                               conn_tx_ctx_t *tx_ctx) {
    unsigned char hdr_bytes[FTX2_HEADER_LEN];
    char body[2048]; /* large enough for QUERY_TX: outer JSON (~100B) + embedded record (~512B) */
    char request_body[1024];
    ftx2_header_t hdr;
    ftx2_tx_meta_t meta;
    const char *extra = NULL;
    uint64_t extra_len = 0;

    if (!state) return -1;
    if (recv_exact(client_fd, hdr_bytes, sizeof(hdr_bytes)) != 0) return -1;

    hdr.magic      = read_le32(hdr_bytes + 0);
    hdr.version    = read_le16(hdr_bytes + 4);
    hdr.frame_type = read_le16(hdr_bytes + 6);
    hdr.flags      = read_le32(hdr_bytes + 8);
    hdr.body_len   = read_le64(hdr_bytes + 12);
    hdr.trace_id   = read_le64(hdr_bytes + 20);

    if (hdr.magic != FTX2_MAGIC) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id, "bad_magic", 9);
    }
    if (hdr.version != FTX2_VERSION) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id, "bad_version", 11);
    }

    /* Per-frame elevation retry. If the startup ucred jailbreak
     * failed (kernel R/W wasn't available — typically the user
     * loaded our payload before kstuff), this is the path that
     * picks up R/W as soon as kstuff lands. The function early-
     * outs in the steady-state already-elevated case, so the
     * cost is one branch on the hot path. */
    runtime_apply_ucred_jailbreak();

    /* Reject wrong-port frames. We can't safely process them: a misrouted
     * STREAM_SHARD would carry up to 32 MiB we'd otherwise drain, and
     * processing a BEGIN_TX on the mgmt port would corrupt the transfer
     * thread's tx table. Emit ERROR and close so the client reconnects
     * to the correct port. */
    if (is_transfer_frame_type(hdr.frame_type) != !!is_transfer_port) {
        (void)send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                         "wrong_port", 10);
        return -1;
    }

    /*
     * STREAM_SHARD bodies can be up to 32 MiB — dispatch before the
     * 1024-byte body buffer guard; the shard handler reads its own body.
     */
    if (hdr.frame_type == FTX2_FRAME_STREAM_SHARD) {
        return handle_stream_shard(state, client_fd, hdr.trace_id, hdr.body_len);
    }

    /*
     * BEGIN_TX carries a manifest JSON that grows linearly with file count.
     * Heap-allocate the body so large directories don't hit the stack guard.
     */
    if (hdr.frame_type == FTX2_FRAME_BEGIN_TX) {
        /* Hard cap on the BEGIN_TX manifest size. PPSA01342 (the
         * largest small-file workload we validate against) has 223k
         * files at ~150 bytes per manifest entry → ~33 MB; the cap
         * leaves an order-of-magnitude headroom for any real upload
         * while refusing absurd values (e.g., crafted body_len of
         * several GB) that would otherwise either succeed at malloc
         * and OOM the payload, or fail malloc and tie up a worker
         * thread draining bytes from a malicious LAN client until
         * recv_exact times out. Above this cap we close the connection
         * (via -1 return) since accepting further frames after a
         * misaligned drain isn't safe. */
        #define BEGIN_TX_BODY_MAX ((uint64_t)256 * 1024 * 1024)
        char resp[2048];
        char *begin_body = NULL;
        uint64_t begin_body_len = hdr.body_len;
        const char *bextra = NULL;
        uint64_t bextra_len = 0;
        ftx2_tx_meta_t bmeta;
        runtime_tx_entry_t *entry = NULL;
        int ret;
        int len;

        if (begin_body_len > BEGIN_TX_BODY_MAX) {
            (void)send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                             "begin_tx_body_too_large", 23);
            return -1;
        }

        if (begin_body_len > 0) {
            begin_body = (char *)malloc((size_t)begin_body_len + 1);
            if (!begin_body) {
                /* drain and report OOM */
                uint64_t rem = begin_body_len;
                char discard[256];
                while (rem > 0) {
                    size_t take = rem > sizeof(discard) ? sizeof(discard) : (size_t)rem;
                    if (recv_exact(client_fd, discard, take) != 0) return -1;
                    rem -= (uint64_t)take;
                }
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                  "out_of_memory", 13);
            }
            if (recv_exact(client_fd, begin_body, (size_t)begin_body_len) != 0) {
                free(begin_body);
                return -1;
            }
            begin_body[begin_body_len] = '\0';
        }

        if (parse_tx_meta(begin_body ? begin_body : "", begin_body_len,
                          &bmeta, &bextra, &bextra_len) != 0) {
            free(begin_body);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "invalid_tx_meta", 15);
        }
        /* Resolve the tx entry against the journal, picking one of three
         * outcomes per (has-existing-entry) × (client-wants-resume):
         *
         *   existing + resume-flag:  RESUME — adopt, preserve shards_received,
         *                            tmp file, and manifest heap. (The entry
         *                            state may be either "interrupted" (clean
         *                            drop, payload saw it) or "active" (TCP
         *                            drop, payload's recv returned -1 and we
         *                            marked it interrupted in the connection
         *                            cleanup OR we didn't get a chance to yet
         *                            — both imply the client's partial data is
         *                            still on disk.)
         *
         *   existing + no-resume:    RESTART — client explicitly asked for a
         *                            fresh tx. Drop tmp, reset counters. This
         *                            catches "I cancelled and want to start
         *                            over" where the client intentionally
         *                            reuses the tx_id.
         *
         *   no existing:             FRESH — normal new tx allocation.
         *
         * `is_resume` drives whether later code in this handler re-runs
         * destructive side-effects: unlinking kind=1 tmp files and
         * re-parsing the kind=2 manifest index. Both must be skipped on
         * resume — they'd destroy the very state we're trying to adopt. */
        int is_resume = 0;
        int want_resume = (bmeta.flags & FTX2_TX_FLAG_RESUME) != 0;
        /* New in P3: client opts in to APPLY_PROGRESS frames during the
         * multi-file commit apply loop. The flag is captured into the
         * tx entry below (both FRESH and RESUME paths) so the apply
         * loop can check entry->apply_progress_enabled when deciding
         * whether to emit. Opt-in keeps old clients safe — they don't
         * set the flag, so we stay silent and they see the legacy
         * single-CommitTxAck behaviour. */
        int want_apply_progress =
            (bmeta.flags & FTX2_TX_FLAG_APPLY_PROGRESS_REQUESTED) != 0;
        pthread_mutex_lock(&state->state_mtx);
        {
            runtime_tx_entry_t *existing = runtime_find_tx_entry(state, bmeta.tx_id);
            if (existing && want_resume) {
                /* Adopt. If state is "interrupted" we're coming back from
                 * a paused tx — the mark-interrupted path decremented
                 * `active_transactions` on pause, so bump it back now
                 * so the matching commit/abort decrement balances. If
                 * state is already "active" (TCP drop raced the first
                 * reconnect before mark-interrupted could fire), the
                 * counter already reflects this tx — don't double-count. */
                int was_interrupted = strcmp(existing->state, "interrupted") == 0;
                snprintf(existing->state, sizeof(existing->state), "active");
                if (was_interrupted) {
                    state->active_transactions += 1;
                }
                entry = existing;
                is_resume = 1;
            } else if (existing && !want_resume) {
                /* Explicit restart with same tx_id — common when the user
                 * cancels an upload and re-clicks Override. Drop partial
                 * state and reuse the slot. Counter accounting mirrors
                 * the want_resume branch: if the prior state was
                 * "interrupted" we're re-activating, so bump. If it was
                 * still "active" the counter already reflects this tx
                 * (TCP drop raced, mark-interrupted didn't fire). */
                int was_interrupted = strcmp(existing->state, "interrupted") == 0;
                entry = existing;
                runtime_release_tx_resources(entry); /* unlinks tmp */
                entry->shards_received = 0;
                entry->bytes_received  = 0;
                entry->total_shards    = 0;
                entry->total_bytes     = 0;
                entry->file_count      = 0;
                entry->direct_mode     = 0;
                entry->multi_file      = 0;
                entry->tmp_path[0]     = '\0';
                entry->dest_root[0]    = '\0';
                snprintf(entry->state, sizeof(entry->state), "active");
                if (was_interrupted) {
                    state->active_transactions += 1;
                }
                is_resume = 0;
            } else {
                /* Fresh allocation. */
                state->active_transactions += 1;
                state->last_tx_seq += 1;
                entry = runtime_alloc_tx_entry(state, bmeta.tx_id, state->last_tx_seq);
                is_resume = 0;
            }
        }
        pthread_mutex_unlock(&state->state_mtx);
        if (!entry) {
            /* Roll back the optimistic active_transactions increment we
             * did before calling runtime_alloc_tx_entry. Without this,
             * every tx_table_full rejection permanently inflates the
             * counter, corrupting STATUS_ACK and the crash-recovery
             * journal's "active_transactions=" field. */
            pthread_mutex_lock(&state->state_mtx);
            if (state->active_transactions > 0) state->active_transactions -= 1;
            pthread_mutex_unlock(&state->state_mtx);
            free(begin_body);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_table_full", 13);
        }
        /* Skip manifest/metadata extraction on resume — the entry already
         * carries the correct values from the original BEGIN_TX (either
         * still in memory, or reloaded from the journal on payload start).
         * Re-extracting would be idempotent if the client re-sends the
         * exact same body, but it would overwrite our dest_root/total_shards
         * with whatever the client decides to send, and there's no upside. */
        if (!is_resume && bextra && bextra_len > 0) {
            extract_json_string_field(bextra, "dest_root",
                                      entry->dest_root, sizeof(entry->dest_root));
            /* Reject manifests whose dest_root would let a LAN client
             * write outside the allowlisted writable roots
             * (/data, /user, /mnt/ext_, /mnt/usb_, /mnt/ps5upload/_).
             * Without this, a crafted BEGIN_TX with dest_root of
             * "/system_ex/..." or "/system_data/priv/..." passes
             * straight into the open()+write() path further down with
             * no further validation. The directory traversal check is
             * inside is_path_allowed (component-scoped, rejects "..").
             */
            if (entry->dest_root[0] && !is_path_allowed(entry->dest_root)) {
                /* runtime_abort_tx_fatal handles the state.active_transactions
                 * decrement, marks state="aborted", flushes the journal
                 * record, and releases tmp/manifest resources — the
                 * complete cleanup path. The earlier draft just unlocked
                 * the per-slot mutex via runtime_release_tx_entry, which
                 * left in_use=1 and active_transactions over-counted —
                 * the slot would leak until the periodic janitor sweep
                 * (or never, since journal flush hadn't yet run). */
                runtime_abort_tx_fatal(state, entry);
                free(begin_body);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0,
                                  hdr.trace_id,
                                  "dest_root_not_allowed", 21);
            }
            entry->total_shards = extract_json_uint64_field(bextra, "total_shards");
            entry->total_bytes  = extract_json_uint64_field(bextra, "total_bytes");
            entry->file_count   = extract_json_uint64_field(bextra, "file_count");
            if (entry->file_count == 0) entry->file_count = 1;
            /* Authoritative single-vs-multi-file decision: the BEGIN_TX kind,
             * NOT file_count. kind==2 is a multi-file (folder) transaction
             * even when it carries exactly one file — see runtime_tx_entry_t
             * ::multi_file. The engine packs lone small files, so this MUST
             * be set for the packed-shard path to accept them. */
            entry->multi_file   = (bmeta.kind == 2);
            (void)runtime_write_manifest(entry, bextra, (size_t)bextra_len);
        }
        /* P3: stamp the per-tx apply-progress opt-in from BEGIN_TX flags
         * onto the entry. Set on every BEGIN_TX (including resume — the
         * client can change its mind across attempts; an old client
         * resuming a new-client tx would clear the flag, expectedly).
         * Only meaningful for multi_file txs since single-file commits
         * are fast and don't need progress reporting. */
        entry->apply_progress_enabled = want_apply_progress;
        /* Enable direct-write path:
         *   - single-file (kind=1, file_count<=1): stream straight to
         *     <dest>.ps5up2-tmp, rename at commit.
         *   - multi-file (kind=2, files[] populated): stream each shard to
         *     <file_path>.ps5up2-tmp using the manifest to route shard_seq
         *     to its file. Rename each tmp at commit.
         *
         * Shards arrive in sequence within a single connection so per-file
         * append ordering is guaranteed.
         *
         * On resume: direct_mode / tmp_path / manifest_blob / manifest_index
         * are already set from the original BEGIN_TX, so both branches
         * short-circuit. The unlink in the kind=1 branch and the manifest
         * rebuild in the kind=2 branch are the bugs the `is_resume` guard
         * is here to prevent — both would destroy the partial state that
         * shards_received is about to tell the client to skip past.
         */
        if (is_resume) {
            /* In-memory resume (TCP drop, payload still running): the entry
             * already carries manifest_index + direct_mode, so the restore
             * blocks below are no-ops and we go straight to reconcile.
             *
             * Resume AFTER a payload restart (power loss / takeover): the
             * startup load restored only scalar fields — manifest_index is
             * NULL and direct_mode is 0. Without restoring them, a multi-
             * file tx would fall to the spool commit path with its shards
             * missing (they're in tmps from the original direct run) and
             * fail; a single-file tx would append at the wrong offset.
             * Rebuild the direct-mode state from the ON-DISK manifest (the
             * same builder the fresh path uses) so the resumed transfer
             * continues on the verified direct path. */
            if (entry->multi_file && !entry->manifest_index) {
                char *mbuf = NULL;
                size_t mlen = 0;
                int from_begin = 0;
                /* 2.25.x: PREFER the manifest carried in THIS resume BeginTx
                 * over the stale on-disk journal copy. Since per-file durable
                 * promote landed, the engine reconciles before every resume and
                 * EXCLUDES the files that already promoted to their final path,
                 * so the BeginTx manifest is REDUCED (fewer files, shards
                 * renumbered 1..N). Rebuilding from the journal's ORIGINAL
                 * full manifest would route the reduced stream by the old
                 * numbering — "no manifest file owns shard <n>" → fatal abort,
                 * the exact same-tx-id resume failure the promote change
                 * otherwise introduces. Adopt the engine's current manifest
                 * (authoritative — it already accounts for what's durable),
                 * refresh the scalar totals from it, and re-persist the journal
                 * so a further restart stays consistent. The resume cursor is a
                 * COUNT against the OLD numbering and is meaningless here, but
                 * reconcile_resume_cursor (called just below) re-derives it from
                 * on-disk durable state: every file in a freshly-reduced
                 * manifest is non-durable, so it clamps the cursor to 0 and the
                 * whole reduced set is re-requested cleanly. Fall back to the
                 * journal copy only when this BeginTx carried no manifest (an
                 * older client that doesn't resend it on resume). */
                if (bextra && bextra_len > 0) {
                    uint64_t fc = extract_json_uint64_field(bextra, "file_count");
                    uint64_t ts = extract_json_uint64_field(bextra, "total_shards");
                    uint64_t tb = extract_json_uint64_field(bextra, "total_bytes");
                    mbuf = (char *)malloc((size_t)bextra_len + 1);
                    if (mbuf) {
                        memcpy(mbuf, bextra, (size_t)bextra_len);
                        mbuf[bextra_len] = '\0';
                        mlen = (size_t)bextra_len;
                        from_begin = 1;
                        if (fc > 0) entry->file_count = fc;
                        entry->total_shards = ts;
                        entry->total_bytes  = tb;
                    }
                }
                if (!mbuf && runtime_read_manifest_alloc(entry, &mbuf, &mlen) != 0) {
                    mbuf = NULL;
                }
                if (mbuf) {
                    manifest_index_entry_t *ridx = NULL;
                    uint64_t ridx_count = 0;
                    if (build_manifest_index(mbuf, mlen, entry->file_count,
                                             &ridx, &ridx_count) == 0) {
                        entry->manifest_blob        = mbuf;
                        entry->manifest_blob_len    = mlen;
                        entry->manifest_index       = ridx;
                        entry->manifest_index_count = ridx_count;
                        entry->direct_mode          = 1;
                        if (from_begin) {
                            /* Persist the adopted (reduced) manifest so a
                             * SECOND restart rebuilds from it, not the
                             * original. */
                            (void)runtime_write_manifest(entry, mbuf, mlen);
                            /* CRITICAL (2.25.1): reset the cursor to 0 and
                             * re-send the whole reduced manifest from scratch.
                             * The engine reconciled before this resume, so
                             * every file here is one it wants RE-SENT (durably-
                             * promoted files were excluded). The journaled
                             * shards_received is a COUNT against the OLD full-
                             * manifest numbering — meaningless against the
                             * renumbered reduced set. Worse: each per-file tmp
                             * was posix_fallocate()'d to its FULL size on its
                             * first shard, so a half-written in-flight file
                             * stat()s at exactly its manifest size, and
                             * reconcile_resume_cursor's "tmp at exact size =
                             * durable" test FALSELY marks it complete and skips
                             * it → the in-flight file lands CORRUPT (confirmed
                             * on hardware: a mid-file crash + resume produced a
                             * 0%-matching big.bin). Resetting to 0 makes the
                             * engine re-send all reduced-manifest shards; each
                             * file's first shard unlinks + re-truncates its
                             * tmp, so the write is clean. This exactly mirrors
                             * the single-file resume path below. Cost: re-send
                             * the one or two not-yet-complete files from their
                             * start (completed files are already promoted to
                             * their final path and excluded). */
                            entry->shards_received = 0;
                            entry->bytes_received  = 0;
                        }
                    } else {
                        free(mbuf);
                    }
                }
            } else if (!entry->multi_file && !entry->direct_mode &&
                       entry->dest_root[0]) {
                /* Single-file resume after a restart. We can't precisely map
                 * on-disk bytes back to a shard boundary without the shard
                 * size, so the safe move is a full re-send: restore direct
                 * mode + tmp path, drop the partial tmp, reset the cursor.
                 * Correct and self-healing; cost is re-uploading one file. */
                entry->direct_mode = 1;
                snprintf(entry->tmp_path, sizeof(entry->tmp_path),
                         "%s.ps5up2-tmp", entry->dest_root);
                (void)unlink(entry->tmp_path);
                entry->shards_received = 0;
                entry->bytes_received  = 0;
            }
            /* Reconcile the multi-file cursor against what's durably on disk
             * (see reconcile_resume_cursor). A no-op when everything up to
             * the journaled cursor is present (the in-memory case); on a
             * restart it pulls the cursor back to the last fully-written
             * file so the gap gets re-sent instead of skipped. */
            if (entry->multi_file) {
                reconcile_resume_cursor(entry);
            }
        } else if (bmeta.kind == 1 && entry->file_count <= 1 && entry->dest_root[0]) {
            entry->direct_mode = 1;
            snprintf(entry->tmp_path, sizeof(entry->tmp_path),
                     "%s.ps5up2-tmp", entry->dest_root);
            if (ensure_parent_dir(entry->dest_root) != 0) {
                fprintf(stderr, "[payload2] direct: ensure_parent_dir failed: %s\n",
                        entry->dest_root);
                entry->direct_mode = 0;
            } else {
                /* Fresh tx: unlink any leftover tmp. Not reached on resume. */
                (void)unlink(entry->tmp_path);
            }
        } else if (bmeta.kind == 2 && entry->file_count > 0 &&
                   bextra && bextra_len > 0 &&
                   strstr(bextra, "\"files\":[")) {
            /* Multi-file direct mode: parse the manifest once into a heap
             * index so per-shard routing is O(log N), not O(N) per shard.
             * If blob exceeds the hard cap or the parse fails, we refuse
             * the transaction outright — the old code silently truncated a
             * multi-KiB manifest into an 8 KiB stack buffer per shard, which
             * corrupted any transfer with more than ~60 files. */
            if (bextra_len > PS5UPLOAD2_MAX_MANIFEST_BLOB) {
                runtime_abort_tx_fatal(state, entry);
                free(begin_body);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                  "manifest_too_large", 18);
            }
            {
                char *blob = (char *)malloc((size_t)bextra_len + 1);
                manifest_index_entry_t *idx = NULL;
                uint64_t idx_count = 0;
                if (!blob) {
                    runtime_abort_tx_fatal(state, entry);
                    free(begin_body);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                      "out_of_memory", 13);
                }
                memcpy(blob, bextra, (size_t)bextra_len);
                blob[bextra_len] = '\0';
                if (build_manifest_index(blob, (size_t)bextra_len,
                                         entry->file_count, &idx, &idx_count) != 0) {
                    free(blob);
                    runtime_abort_tx_fatal(state, entry);
                    free(begin_body);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                      "manifest_invalid", 16);
                }
                entry->manifest_blob         = blob;
                entry->manifest_blob_len     = (size_t)bextra_len;
                entry->manifest_index        = idx;
                entry->manifest_index_count  = idx_count;
                entry->direct_mode = 1;
                /* Unlink any stale per-file .ps5up2-tmp files left by a
                 * prior aborted/failed upload of the same destination.
                 * The single-file path at line 7270 already does this for
                 * its single tmp; multi-file mode was missing the
                 * equivalent sweep, which produced a hard-to-spot silent
                 * corruption pattern:
                 *
                 *   1. Prior upload of game G fails partway, some
                 *      <file>.ps5up2-tmp files remain on disk.
                 *   2. User retries upload of game G. Engine's pack
                 *      decision for some files differs from last time
                 *      (because pack threshold or size near boundary),
                 *      OR the file is now packed where it was non-packed.
                 *   3. Pack worker writes correct content to file_path
                 *      directly. No new tmp created for that file.
                 *   4. COMMIT rename loop iterates manifest entries,
                 *      sees the STALE <file>.ps5up2-tmp from step 1,
                 *      rename(stale_tmp, file_path) succeeds — and
                 *      OVERWRITES the just-written-correct packed content
                 *      with the stale tmp. Client sees commit success,
                 *      destination has stale content, game won't launch.
                 *
                 * Sweeping at fresh BEGIN_TX time eliminates the
                 * stale-tmp source. The is_resume branch above
                 * intentionally skips this sweep — a true resume needs
                 * the partial tmps preserved. */
                {
                    const manifest_index_entry_t *idx_entries =
                        (const manifest_index_entry_t *)idx;
                    for (uint64_t i = 0; i < idx_count; i++) {
                        char file_path[512];
                        char stale_tmp[512 + 16];
                        size_t plen = idx_entries[i].path_len;
                        uint32_t poff = idx_entries[i].path_offset;
                        if (plen == 0 || plen >= sizeof(file_path)) continue;
                        /* Defense-in-depth bounds check matching
                         * lookup_manifest_index — build_manifest_index
                         * already validates these, but the cost is one
                         * comparison and it makes this loop safe under
                         * any future manifest-builder change. */
                        if ((uint64_t)poff + (uint64_t)plen > bextra_len) {
                            continue;
                        }
                        if (json_copy_unescaped_string(blob + poff,
                                                       blob + poff + plen,
                                                       file_path,
                                                       sizeof(file_path)) != 0) {
                            continue;
                        }
                        snprintf(stale_tmp, sizeof(stale_tmp),
                                 "%s.ps5up2-tmp", file_path);
                        (void)unlink(stale_tmp);
                    }
                }
            }
        }
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "begin_tx");
        (void)runtime_flush_tx_record(state, entry);
        free(begin_body);
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);

        len = snprintf(resp, sizeof(resp),
                       "{\"accepted\":true,\"tx_id\":\"%s\",\"tx_seq\":%llu,"
                       "\"active_transactions\":%llu,"
                       "\"last_acked_shard\":%llu}",
                       entry->tx_id_hex,
                       (unsigned long long)entry->tx_seq,
                       (unsigned long long)state->active_transactions,
                       (unsigned long long)entry->shards_received);
        if (len < 0) return -1;
        /* Remember this tx on the connection so a socket close before
         * COMMIT/ABORT marks it "interrupted" rather than leaving an
         * "active" orphan in the journal. */
        if (tx_ctx) {
            memcpy(tx_ctx->tx_id, entry->tx_id, 16);
            tx_ctx->has_tx = 1;
        }
        ret = send_frame(client_fd, FTX2_FRAME_BEGIN_TX_ACK, 0, hdr.trace_id,
                         resp, (uint64_t)len);
        return ret;
    }

    if (hdr.frame_type == FTX2_FRAME_FS_WRITE_BYTES ||
        hdr.frame_type == FTX2_FRAME_TOAST_SEND) {
        uint64_t body_cap = hdr.frame_type == FTX2_FRAME_FS_WRITE_BYTES
            ? (uint64_t)512 * 1024
            : (uint64_t)4096;
        char *large_body = NULL;
        if (hdr.body_len > body_cap) {
            uint64_t remaining = hdr.body_len;
            char discard[256];
            uint16_t ack_frame = hdr.frame_type == FTX2_FRAME_FS_WRITE_BYTES
                ? FTX2_FRAME_FS_WRITE_BYTES_ACK
                : FTX2_FRAME_TOAST_SEND_ACK;
            const char *ack = "{\"ok\":false,\"err\":\"body_too_large\"}";
            while (remaining > 0) {
                size_t take = remaining > sizeof(discard)
                    ? sizeof(discard)
                    : (size_t)remaining;
                if (recv_exact(client_fd, discard, take) != 0) return -1;
                remaining -= (uint64_t)take;
            }
            return send_frame(client_fd, ack_frame, 0, hdr.trace_id,
                              ack, strlen(ack));
        }
        if (hdr.body_len > 0) {
            large_body = (char *)malloc((size_t)hdr.body_len + 1);
            if (!large_body) {
                uint64_t remaining = hdr.body_len;
                char discard[256];
                uint16_t ack_frame = hdr.frame_type == FTX2_FRAME_FS_WRITE_BYTES
                    ? FTX2_FRAME_FS_WRITE_BYTES_ACK
                    : FTX2_FRAME_TOAST_SEND_ACK;
                const char *ack = "{\"ok\":false,\"err\":\"out_of_memory\"}";
                while (remaining > 0) {
                    size_t take = remaining > sizeof(discard)
                        ? sizeof(discard)
                        : (size_t)remaining;
                    if (recv_exact(client_fd, discard, take) != 0) return -1;
                    remaining -= (uint64_t)take;
                }
                return send_frame(client_fd, ack_frame, 0, hdr.trace_id,
                                  ack, strlen(ack));
            }
            if (recv_exact(client_fd, large_body, (size_t)hdr.body_len) != 0) {
                free(large_body);
                return -1;
            }
            large_body[hdr.body_len] = '\0';
        }
        if (hdr.frame_type == FTX2_FRAME_FS_WRITE_BYTES) {
            int rc = handle_fs_write_bytes(state, client_fd, hdr.trace_id,
                                           large_body ? large_body : "",
                                           hdr.body_len);
            free(large_body);
            return rc;
        } else {
            int rc = handle_toast_send(state, client_fd, hdr.trace_id,
                                       large_body ? large_body : "",
                                       hdr.body_len);
            free(large_body);
            return rc;
        }
    }

    /* Read the body for all other (small) frame types. */
    request_body[0] = '\0';
    if (hdr.body_len > 0) {
        if (hdr.body_len >= sizeof(request_body)) {
            uint64_t remaining = hdr.body_len;
            char discard[256];
            while (remaining > 0) {
                size_t take = remaining > sizeof(discard) ? sizeof(discard) : (size_t)remaining;
                if (recv_exact(client_fd, discard, take) != 0) return -1;
                remaining -= (uint64_t)take;
            }
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "body_too_large", 14);
        }
        if (recv_exact(client_fd, request_body, (size_t)hdr.body_len) != 0) return -1;
        request_body[hdr.body_len] = '\0';
    }

    /* command_count is a cross-thread counter (incremented on both the
     * transfer and mgmt listeners). The mutex window is a single add,
     * so contention is negligible and a torn read from STATUS is
     * prevented entirely. */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    /* ── HELLO ── */
    if (hdr.frame_type == FTX2_FRAME_HELLO) {
        int len = snprintf(body, sizeof(body),
                           "{\"version\":%u,\"instance_id\":%llu,\"runtime_port\":%d}",
                           FTX2_VERSION,
                           (unsigned long long)state->instance_id,
                           state->runtime_port);
        /* snprintf returns the length that *would* have been written
         * (excluding NUL). A return > sizeof(body) means truncation;
         * clamp so we don't ask send_frame to read past the buffer. */
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_HELLO_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── STATUS ── */
    if (hdr.frame_type == FTX2_FRAME_STATUS) {
        /* Snapshot cross-thread fields under the mutex so a concurrent
         * transfer-side BEGIN/COMMIT/ABORT can't produce a mid-flight
         * read of active_transactions or last_tx_seq. Keep the lock
         * window to memory copies only — never hold across send_frame. */
        uint64_t snap_instance_id, snap_started_at, snap_command_count;
        uint64_t snap_active_tx, snap_last_seq, snap_recovered;
        int snap_shutdown, snap_startup_reason, snap_takeover_req, snap_port;
        int len;
        char kernel_version_raw[256];
        char kernel_version_esc[512];
        read_ps5_kernel_version(kernel_version_raw, sizeof(kernel_version_raw));
        json_escape_into(kernel_version_raw, kernel_version_esc, sizeof(kernel_version_esc));
        pthread_mutex_lock(&state->state_mtx);
        snap_instance_id    = state->instance_id;
        snap_port           = state->runtime_port;
        snap_shutdown       = state->shutdown_requested;
        snap_startup_reason = state->startup_reason;
        snap_takeover_req   = state->takeover_requested;
        snap_started_at     = state->started_at_unix;
        snap_command_count  = state->command_count;
        snap_active_tx      = state->active_transactions;
        snap_last_seq       = state->last_tx_seq;
        snap_recovered      = state->recovered_transactions;
        pthread_mutex_unlock(&state->state_mtx);
        /* Surface ucred elevation result so the client UI can warn
         * "load kstuff first" when elevation == false without
         * having to call a Sony API that might wedge. The pid -1
         * value used in main()'s call means "current process";
         * 0 from the kernel = elevation succeeded.
         * `g_ucred_elevation_rc` is defined in main.c. */
        extern volatile int g_ucred_elevation_rc;
        const int ucred_elevated = (g_ucred_elevation_rc == 0) ? 1 : 0;
        len = snprintf(body, sizeof(body),
                       "{\"version\":\"%s\","
                       "\"ps5_kernel\":\"%s\","
                       "\"instance_id\":%llu,\"runtime_port\":%d,"
                       "\"shutdown\":%d,\"startup_reason\":%d,"
                       "\"takeover_requested\":%d,\"started_at_unix\":%llu,"
                       "\"command_count\":%llu,\"active_transactions\":%llu,"
                       "\"last_tx_seq\":%llu,\"recovered_transactions\":%llu,"
                       "\"ucred_elevated\":%s,"
                       /* Multi-stream capability: how many parallel transfer
                        * connections this payload will service concurrently.
                        * Absent on old payloads → engine treats it as 1. */
                       "\"max_transfer_streams\":%d}",
                       PS5UPLOAD2_VERSION,
                       kernel_version_esc,
                       (unsigned long long)snap_instance_id,
                       snap_port,
                       snap_shutdown,
                       snap_startup_reason,
                       snap_takeover_req,
                       (unsigned long long)snap_started_at,
                       (unsigned long long)snap_command_count,
                       (unsigned long long)snap_active_tx,
                       (unsigned long long)snap_last_seq,
                       (unsigned long long)snap_recovered,
                       ucred_elevated ? "true" : "false",
                       PS5UPLOAD2_TRANSFER_STREAMS_ADVERTISED);
        /* Truncation-safe: if the fields ever grow past `body`, clamp
         * rather than emit a body_len that drives send_frame to read
         * past the stack buffer. The JSON is still valid-on-arrival
         * only when len < sizeof(body); the engine will fail the
         * serde_json::from_slice on a truncated body and return 502
         * to the UI, which is the right failure mode. */
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_STATUS_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── QUERY_TX ── */
    if (hdr.frame_type == FTX2_FRAME_QUERY_TX) {
        char record[1024];
        int len;
        int requested_specific = 0;
        int have_snapshot = 0;
        uint64_t snap_active = 0;
        uint64_t snap_last = 0;
        runtime_tx_entry_t snap_entry;
        memset(&snap_entry, 0, sizeof(snap_entry));

        /* Snapshot the state we need under the lock, then release the
         * lock before doing I/O. The transfer side takes this mutex on
         * every SHARD ACK — holding it across `runtime_read_tx_record`
         * (which does fopen+fread on a UFS-backed file that can stall
         * tens of milliseconds under pressure) was dropping shards
         * mid-flight because the writer thread blocked on the lock
         * past its recv timeout. */
        pthread_mutex_lock(&state->state_mtx);
        {
            runtime_tx_entry_t *entry = NULL;
            snap_active = state->active_transactions;
            snap_last   = state->last_tx_seq;
            if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) == 0) {
                requested_specific = 1;
                entry = runtime_find_tx_entry(state, meta.tx_id);
            }
            if (!entry && !requested_specific && snap_last > 0) {
                int i = 0;
                for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
                    if (state->tx_entries[i].in_use &&
                        state->tx_entries[i].tx_seq == snap_last) {
                        entry = &state->tx_entries[i];
                        break;
                    }
                }
            }
            if (entry) {
                snap_entry = *entry;   /* copy-by-value while locked */
                have_snapshot = 1;
            }
        }
        pthread_mutex_unlock(&state->state_mtx);

        if (requested_specific && !have_snapshot) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }

        /* File read now happens lock-free. If the entry was evicted
         * between snapshot + read we get a stale or missing record;
         * that's surfaced as `last_tx_record: null` which is what the
         * pre-existing code already did for that case. */
        if (have_snapshot && runtime_read_tx_record(&snap_entry, record, 1024) == 0) {
            len = snprintf(body, sizeof(body),
                           "{\"active_transactions\":%llu,\"last_tx_seq\":%llu,"
                           "\"tx_id\":\"%s\",\"state\":\"%s\","
                           "\"shards_received\":%llu,\"bytes_received\":%llu,"
                           "\"last_tx_record\":%s}",
                           (unsigned long long)snap_active,
                           (unsigned long long)snap_last,
                           snap_entry.tx_id_hex,
                           snap_entry.state,
                           (unsigned long long)snap_entry.shards_received,
                           (unsigned long long)snap_entry.bytes_received,
                           record);
        } else {
            len = snprintf(body, sizeof(body),
                           "{\"active_transactions\":%llu,\"last_tx_seq\":%llu,"
                           "\"last_tx_record\":null}",
                           (unsigned long long)snap_active,
                           (unsigned long long)snap_last);
        }
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_QUERY_TX_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── COMMIT_TX ── */
    if (hdr.frame_type == FTX2_FRAME_COMMIT_TX) {
        int len;
        int rc;
        runtime_tx_entry_t *entry = NULL;
        if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) != 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "invalid_tx_meta", 15);
        }
        /* Acquire exclusive: a concurrent SHARD on the transfer port
         * must finish before we tear down the manifest/writer state.
         * Released via the `commit_done:` cleanup. */
        entry = runtime_acquire_tx_entry(state, meta.tx_id);
        if (!entry) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }
        if (strcmp(entry->state, "active") != 0) {
            rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                            "tx_not_active", 13);
            goto commit_done;
        }
        /* Verify all expected shards arrived (skip check if total_shards unknown). */
        if (entry->total_shards > 0 &&
            entry->shards_received < entry->total_shards) {
            len = snprintf(body, sizeof(body),
                           "{\"error\":\"shards_incomplete\","
                           "\"shards_received\":%llu,\"total_shards\":%llu}",
                           (unsigned long long)entry->shards_received,
                           (unsigned long long)entry->total_shards);
            if (len < 0) { rc = -1; goto commit_done; }
            rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                            body, (uint64_t)len);
            goto commit_done;
        }
        pthread_mutex_lock(&state->state_mtx);
        if (state->active_transactions > 0) state->active_transactions -= 1;
        pthread_mutex_unlock(&state->state_mtx);
        /* We hold the per-slot mutex, so we can mutate entry->state
         * directly. */
        snprintf(entry->state, sizeof(entry->state), "%s", "committed");
        (void)runtime_flush_tx_record(state, entry);
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "commit_tx");
        /* Apply: direct-write tx just renames its tmp file(s); spool tx copies
         * shards into the destination as before.
         *
         * If apply fails (writer reported a disk I/O error mid-stream, or any
         * rename failed), we DO NOT silently report COMMIT_TX_ACK. The
         * pre-2.2.28 code logged a WARN and proceeded with the rename anyway,
         * which destroyed the prior good dest_root and replaced it with a
         * partial/corrupt file while telling the client "success". On
         * apply failure we now emit FTX2_FRAME_ERROR with a structured body,
         * mark the journal state "apply_failed", preserve the tmp file(s) for
         * user inspection, and return early — the destination at dest_root
         * is left untouched. */
        int apply_failed = 0;
        const char *apply_failure_reason = NULL;
        char apply_failure_detail[256] = {0};
        uint64_t failed_renames = 0;
        {
            uint64_t ta0 = now_us();
            if (entry->direct_mode && !entry->multi_file) {
                /* Drain the persistent writer + close the fd before rename.
                 * direct_writer_finish also clears direct_fd / direct_writer,
                 * so the upcoming runtime_release_tx_resources call will not
                 * unlink our freshly-renamed dest_root. */
                int finish_rc = direct_writer_finish(entry);
                if (finish_rc != 0) {
                    /* Writer thread reported a write_full() failure during
                     * streaming. The tmp file at entry->tmp_path is
                     * partial/corrupt; refuse the rename and surface the
                     * error to the client. */
                    apply_failed = 1;
                    apply_failure_reason = "direct_writer_io_error";
                    snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                             "writer thread reported a disk write error mid-stream; "
                             "destination preserved, partial at %s",
                             entry->tmp_path);
                    fprintf(stderr,
                            "[payload2] direct writer reported I/O error for tx %s — "
                            "refusing rename, destination unchanged\n",
                            entry->tx_id_hex);
                } else {
                    /* Commit-time integrity check. The shard-count gate
                     * above proves we RECEIVED total_shards shards, but the
                     * resume cursor (last_acked_shard) is also a COUNT, and
                     * during streaming neither the tmp data nor the journal
                     * is fsync'd — so a kill mid-write (PS5 rest-mode, power
                     * loss, takeover) can leave the journaled cursor ahead
                     * of the bytes actually on disk. A later resume then
                     * skips those shards and we'd rename a tmp that's SHORT
                     * of the manifest size: a file that reports "done" but
                     * is missing interior bytes (worst on USB/exFAT, where
                     * page-cache loss is likely). Verify the tmp is exactly
                     * the planned size before publishing it; on a mismatch
                     * refuse the rename and surface a retryable error so the
                     * host shows a failure instead of a false success.
                     * (total_bytes == 0 is the empty-file case handled in
                     * the shard path — skip it. stat() failure is left to
                     * the rename below, which then reports its own error.) */
                    struct stat st_done;
                    if (entry->total_bytes > 0 &&
                        stat(entry->tmp_path, &st_done) == 0 &&
                        (uint64_t)st_done.st_size != entry->total_bytes) {
                        apply_failed = 1;
                        apply_failure_reason = "size_mismatch";
                        snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                                 "destination would be %lld bytes but manifest expects %llu "
                                 "(transfer incomplete — resume cursor outran durable data?); "
                                 "destination preserved, partial at %s",
                                 (long long)st_done.st_size,
                                 (unsigned long long)entry->total_bytes,
                                 entry->tmp_path);
                        fprintf(stderr,
                                "[payload2] commit size mismatch tx %s: tmp=%lld expected=%llu — "
                                "refusing rename, destination unchanged\n",
                                entry->tx_id_hex, (long long)st_done.st_size,
                                (unsigned long long)entry->total_bytes);
                    } else {
                        (void)unlink(entry->dest_root);
                        if (rename(entry->tmp_path, entry->dest_root) != 0) {
                            apply_failed = 1;
                            apply_failure_reason = "direct_rename_failed";
                            snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                                     "rename %s -> %s failed: %s",
                                     entry->tmp_path, entry->dest_root, strerror(errno));
                            fprintf(stderr, "[payload2] %s (errno=%d)\n",
                                    apply_failure_detail, errno);
                        } else {
                            /* Clear tmp_path so the upcoming release_tx_resources
                             * doesn't re-unlink a path that no longer exists. */
                            entry->tmp_path[0] = '\0';
                            printf("[payload2] direct apply rename -> %s\n", entry->dest_root);
                        }
                    }
                }
            } else if (entry->direct_mode && entry->multi_file) {
                uint64_t fi = 0;
                const manifest_index_entry_t *idx =
                    (const manifest_index_entry_t *)entry->manifest_index;
                if (!idx || !entry->manifest_blob) {
                    apply_failed = 1;
                    apply_failure_reason = "manifest_missing_at_commit";
                    snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                             "manifest index/blob unavailable at commit time");
                    fprintf(stderr, "[payload2] direct multi commit: missing manifest index for tx %s\n",
                            entry->tx_id_hex);
                } else {
                    for (fi = 0; fi < entry->manifest_index_count; fi++) {
                        char file_path[512];
                        char tmp_path[512 + 16];
                        size_t plen = idx[fi].path_len;
                        if (plen == 0 || plen >= sizeof(file_path)) continue;
                        if (json_copy_unescaped_string(entry->manifest_blob + idx[fi].path_offset,
                                                       entry->manifest_blob + idx[fi].path_offset + plen,
                                                       file_path,
                                                       sizeof(file_path)) != 0) {
                            continue;
                        }
                        snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", file_path);
                        /* Two legitimate layouts here:
                         *   - non-packed record:  data at tmp_path, rename -> file_path
                         *   - packed record:      data already at file_path, tmp absent
                         * `rename(tmp, dest)` atomically replaces dest on POSIX, so
                         * there is no need for the old defensive `unlink(file_path)`
                         * that this branch used to do — and removing it is what
                         * keeps packed-record content from being wiped. ENOENT on
                         * the rename means "packed path already landed the file at
                         * the destination" which is success, not failure.
                         *
                         * Defense-in-depth pre-check (added 2.2.35): a stale
                         * tmp from a prior aborted run can survive into the
                         * fresh tx if the BEGIN_TX sweep was skipped (resume
                         * path) or missed the tmp due to manifest-vs-disk
                         * path encoding drift. The bug signature is:
                         *   - file_path exists at the manifest's expected
                         *     size (pack worker delivered it correctly OR a
                         *     prior successful run did), AND
                         *   - tmp_path exists but is *smaller* than expected
                         *     (a partial write from a prior aborted run).
                         * In that exact shape, renaming would clobber good
                         * content with stale partial bytes; unlinking the
                         * tmp preserves the correct file.
                         *
                         * The asymmetric size check avoids false positives:
                         *   - User replacing a same-size file → new tmp_path
                         *     holds full new content (size == expected),
                         *     condition does NOT trigger, rename runs.
                         *   - Legitimate resume → by COMMIT all shards
                         *     acked, tmp is at full size; condition does
                         *     NOT trigger.
                         * Only the bug case (full file at dest + partial
                         * stale tmp) hits this guard. */
                        struct stat st_file;
                        struct stat st_tmp;
                        int have_file = (stat(file_path, &st_file) == 0);
                        int have_tmp  = (stat(tmp_path,  &st_tmp)  == 0);
                        if (have_file && have_tmp &&
                            idx[fi].size > 0 &&
                            (uint64_t)st_file.st_size == idx[fi].size &&
                            (uint64_t)st_tmp.st_size  <  idx[fi].size) {
                            fprintf(stderr,
                                    "[payload2] commit: %s already at expected size %llu, tmp partial (%llu) — unlinking stale tmp\n",
                                    file_path,
                                    (unsigned long long)idx[fi].size,
                                    (unsigned long long)st_tmp.st_size);
                            (void)unlink(tmp_path);
                            continue;
                        }
                        if (rename(tmp_path, file_path) != 0 && errno != ENOENT) {
                            fprintf(stderr, "[payload2] direct multi rename %s -> %s errno=%d\n",
                                    tmp_path, file_path, errno);
                            failed_renames += 1;
                        }
                    }
                    if (failed_renames > 0) {
                        apply_failed = 1;
                        apply_failure_reason = "direct_multi_rename_failed";
                        snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                                 "%llu of %llu file rename(s) failed; partials preserved as .ps5up2-tmp",
                                 (unsigned long long)failed_renames,
                                 (unsigned long long)entry->manifest_index_count);
                    } else {
                        /* Commit-time integrity check (multi-file analogue
                         * of the single-file size check above). All renames
                         * succeeded, but the resume cursor is a shard COUNT
                         * and streaming writes aren't fsync'd, so a kill
                         * mid-write (rest-mode / power loss) followed by a
                         * resume can leave a file SHORT of its manifest size
                         * while the count-based gate still passes — a game
                         * that lands looking complete but is silently
                         * corrupt (the AquaHox USB/exFAT case). Re-stat each
                         * file against the manifest size and fail loudly on
                         * any mismatch so the host retries instead of
                         * trusting a false "done". A bare stat on a
                         * just-written file hits the inode cache, so this is
                         * cheap even for a 200k-file game. (size == 0 files
                         * ride inside packed shards and are skipped — same
                         * as the stale-tmp guard above.) */
                        uint64_t bad = 0;
                        char bad_name[256] = {0};
                        uint64_t bad_have = 0;
                        uint64_t bad_want = 0;
                        for (fi = 0; fi < entry->manifest_index_count; fi++) {
                            char vpath[512];
                            size_t vlen = idx[fi].path_len;
                            if (vlen == 0 || vlen >= sizeof(vpath)) continue;
                            if (idx[fi].size == 0) continue;
                            if (json_copy_unescaped_string(
                                    entry->manifest_blob + idx[fi].path_offset,
                                    entry->manifest_blob + idx[fi].path_offset + vlen,
                                    vpath, sizeof(vpath)) != 0) {
                                continue;
                            }
                            struct stat vst;
                            int have = (stat(vpath, &vst) == 0);
                            uint64_t got = have ? (uint64_t)vst.st_size : 0;
                            if (!have || got != idx[fi].size) {
                                if (bad == 0) {
                                    snprintf(bad_name, sizeof(bad_name), "%s", vpath);
                                    bad_have = got;
                                    bad_want = idx[fi].size;
                                }
                                bad += 1;
                            }
                        }
                        if (bad > 0) {
                            apply_failed = 1;
                            apply_failure_reason = "size_mismatch";
                            snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                                     "%llu file(s) wrong size after commit "
                                     "(e.g. %s: have %llu, want %llu) — transfer incomplete "
                                     "(resume cursor outran durable data?)",
                                     (unsigned long long)bad, bad_name,
                                     (unsigned long long)bad_have,
                                     (unsigned long long)bad_want);
                            fprintf(stderr,
                                    "[payload2] commit multi size mismatch tx %s: %llu bad file(s)\n",
                                    entry->tx_id_hex, (unsigned long long)bad);
                        } else {
                            printf("[payload2] direct multi apply: %llu files renamed + verified\n",
                                   (unsigned long long)entry->file_count);
                        }
                    }
                }
            } else if (runtime_apply_spool(entry, client_fd, hdr.trace_id) == 0) {
                (void)runtime_cleanup_spool(entry);
            } else {
                apply_failed = 1;
                apply_failure_reason = "spool_apply_failed";
                snprintf(apply_failure_detail, sizeof(apply_failure_detail),
                         "spool-to-dest apply failed; spool preserved at %s/spool_%s",
                         PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);
                fprintf(stderr, "[payload2] %s — tx %s dest=%s\n",
                        apply_failure_detail, entry->tx_id_hex, entry->dest_root);
            }
            entry->apply_us += (now_us() - ta0);
        }

        /* Apply-failure exit path. Overwrite the journal state to reflect
         * what actually happened on disk, append a distinct event, and
         * surface a structured FTX2_FRAME_ERROR. We deliberately DO NOT
         * call runtime_release_tx_resources(entry) here — keeping the
         * heap state (manifest_blob/index, writer handle if any) lets a
         * subsequent QUERY_TX or RESUME pick up where we left off, and
         * skipping the unlink of tmp_path preserves the bytes we wrote
         * for inspection or manual recovery. */
        if (apply_failed) {
            snprintf(entry->state, sizeof(entry->state), "apply_failed");
            (void)runtime_flush_tx_record(state, entry);
            (void)runtime_save_tx_state(state);
            (void)runtime_append_tx_event(state, "commit_tx_apply_failed");
            /* JSON-escape the detail before interpolating: it embeds
             * strerror(errno) and PS5 paths. Normal PS5 paths don't
             * contain `"` or `\`, but a hostile or buggy `entry->
             * tmp_path` could, and an unescaped backslash or quote
             * would produce malformed JSON the engine's serde_json
             * rejects, masking the original error with a parse error.
             * apply_failure_reason is a static-string code from this
             * function (e.g. "direct_writer_io_error") so it's
             * safe-ASCII; tx_id_hex is hex; only detail needs escape. */
            char detail_esc[512];
            json_escape_into(apply_failure_detail, detail_esc, sizeof(detail_esc));
            len = snprintf(body, sizeof(body),
                           "{\"error\":\"%s\",\"tx_id\":\"%s\","
                           "\"failed_renames\":%llu,"
                           "\"detail\":\"%s\"}",
                           apply_failure_reason ? apply_failure_reason : "apply_failed",
                           entry->tx_id_hex,
                           (unsigned long long)failed_renames,
                           detail_esc);
            if (len < 0) { rc = -1; goto commit_done; }
            if ((size_t)len >= sizeof(body)) len = (int)sizeof(body) - 1;
            /* Commit attempt failed terminally — clear the connection's
             * has-open-tx flag so a subsequent socket close doesn't
             * mark this slot as "interrupted" (overwriting our
             * apply_failed state). */
            if (tx_ctx && tx_ctx->has_tx &&
                memcmp(tx_ctx->tx_id, meta.tx_id, 16) == 0) {
                tx_ctx->has_tx = 0;
            }
            rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                            body, (uint64_t)len);
            goto commit_done;
        }
        /* Manifest blob + index are no longer needed — release the heap
         * immediately rather than waiting for the slot to be evicted. */
        runtime_release_tx_resources(entry);
        fprintf(stderr, "[payload2] tx %s timing(us): recv=%llu write=%llu verify=%llu apply=%llu "
                        "bytes=%llu shards=%llu\n",
                entry->tx_id_hex,
                (unsigned long long)entry->recv_us,
                (unsigned long long)entry->write_us,
                (unsigned long long)entry->verify_us,
                (unsigned long long)entry->apply_us,
                (unsigned long long)entry->bytes_received,
                (unsigned long long)entry->shards_received);
        char dest_root_esc[1024];
        json_escape_into(entry->dest_root, dest_root_esc, sizeof(dest_root_esc));
        len = snprintf(body, sizeof(body),
                       "{\"committed\":true,\"tx_id\":\"%s\",\"tx_seq\":%llu,"
                       "\"shards_received\":%llu,\"bytes_received\":%llu,"
                       "\"dest_root\":\"%s\",\"active_transactions\":%llu,"
                       "\"timing_us\":{\"recv\":%llu,\"write_wait\":%llu,"
                       "\"verify\":%llu,\"apply\":%llu,"
                       "\"open\":%llu,\"join\":%llu,\"close\":%llu,"
                       "\"hash\":%llu,\"shard_fn\":%llu,"
                       "\"pack_records\":%llu,\"pack_unlink\":%llu,"
                       "\"pack_open\":%llu,\"pack_ftruncate\":%llu,"
                       "\"pack_write\":%llu,\"pack_close\":%llu,"
                       "\"pack_open_retries\":%llu,\"pack_write_retries\":%llu},"
                       "\"sock_rcvbuf\":%d,\"listener_rcvbuf_asked\":%d,"
                       "\"listener_rcvbuf_actual\":%d,\"listener_sndbuf_actual\":%d,"
                       "\"max_rcvbuf_probed\":%d}",
                       entry->tx_id_hex,
                       (unsigned long long)entry->tx_seq,
                       (unsigned long long)entry->shards_received,
                       (unsigned long long)entry->bytes_received,
                       dest_root_esc,
                       (unsigned long long)state->active_transactions,
                       (unsigned long long)entry->recv_us,
                       (unsigned long long)entry->write_us,
                       (unsigned long long)entry->verify_us,
                       (unsigned long long)entry->apply_us,
                       (unsigned long long)entry->open_us,
                       (unsigned long long)entry->join_us,
                       (unsigned long long)entry->close_us,
                       (unsigned long long)entry->hash_us,
                       (unsigned long long)entry->shard_func_us,
                       (unsigned long long)entry->pack_records,
                       (unsigned long long)entry->pack_unlink_us,
                       (unsigned long long)entry->pack_open_us,
                       (unsigned long long)entry->pack_ftruncate_us,
                       (unsigned long long)entry->pack_write_us,
                       (unsigned long long)entry->pack_close_us,
                       (unsigned long long)entry->pack_open_retries,
                       (unsigned long long)entry->pack_write_retries,
                       state->last_client_rcvbuf,
                       state->listener_rcvbuf_asked,
                       state->listener_rcvbuf_actual,
                       state->listener_sndbuf_actual,
                       state->max_rcvbuf_probed);
        if (len < 0) { rc = -1; goto commit_done; }
        /* Commit took the tx to a terminal state; clear the connection's
         * "has open tx" marker so a subsequent socket close doesn't
         * mis-mark it as interrupted. */
        if (tx_ctx && tx_ctx->has_tx &&
            memcmp(tx_ctx->tx_id, meta.tx_id, 16) == 0) {
            tx_ctx->has_tx = 0;
        }
        /* User-visible PS5 toast on successful upload (2.16.1+). Fires a
         * top-right Sony notification on the TV/monitor itself so the user
         * gets confirmation even when the desktop ps5upload window is
         * hidden behind the PS5 system chrome. dlsym in pop_notification
         * silently no-ops on firmwares where the symbol isn't loaded, so
         * this never breaks the commit path. */
        {
            const char *base = strrchr(entry->dest_root, '/');
            base = base ? base + 1 : entry->dest_root;
            char toast[200];
            double mib = (double)entry->bytes_received / (1024.0 * 1024.0);
            const char *unit = "MiB";
            double val = mib;
            if (mib >= 1024.0) { val = mib / 1024.0; unit = "GiB"; }
            if (entry->file_count > 1) {
                snprintf(toast, sizeof(toast),
                         "PS5Upload: %s done (%.1f %s, %llu files)",
                         base, val, unit,
                         (unsigned long long)entry->file_count);
            } else {
                snprintf(toast, sizeof(toast),
                         "PS5Upload: %s done (%.1f %s)",
                         base, val, unit);
            }
            pop_notification(toast);
        }
        rc = send_frame(client_fd, FTX2_FRAME_COMMIT_TX_ACK, 0, hdr.trace_id,
                        body, (uint64_t)len);
commit_done:
        runtime_release_tx_entry(state, entry);
        return rc;
    }

    /* ── ABORT_TX ── */
    if (hdr.frame_type == FTX2_FRAME_ABORT_TX) {
        int len;
        int rc;
        runtime_tx_entry_t *entry = NULL;
        if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) == 0) {
            /* Acquire exclusive: same rationale as COMMIT_TX. A
             * concurrent SHARD must finish before we tear the entry
             * down. Released via the `abort_done:` cleanup. */
            entry = runtime_acquire_tx_entry(state, meta.tx_id);
        }
        if (!entry) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }
        /* Guard against double-finalize. Without this, a replayed
         * ABORT_TX or a confused client that issues both COMMIT and
         * ABORT for the same tx_id will re-flush a terminal entry
         * with state="aborted" — silently overwriting a "committed"
         * record in the journal.
         *
         * NB: state "interrupted" (set when a connection dropped
         * mid-tx without seeing COMMIT/ABORT) MUST stay abortable,
         * otherwise the canonical "drop happened, user clicks
         * Cancel on the dangling tx" flow breaks. Only refuse on
         * already-terminal states (aborted or committed).
         *
         * The entry stays acquired, so the `abort_done:` cleanup
         * still runs. */
        if (strcmp(entry->state, "aborted") == 0 ||
            strcmp(entry->state, "committed") == 0) {
            rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                            "tx_already_terminal", 19);
            goto abort_done;
        }
        pthread_mutex_lock(&state->state_mtx);
        if (state->active_transactions > 0) state->active_transactions -= 1;
        uint64_t active_now = state->active_transactions;
        pthread_mutex_unlock(&state->state_mtx);
        snprintf(entry->state, sizeof(entry->state), "%s", "aborted");
        (void)runtime_flush_tx_record(state, entry);
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "abort_tx");
        runtime_release_tx_resources(entry);
        len = snprintf(body, sizeof(body),
                       "{\"aborted\":true,\"tx_id\":\"%s\","
                       "\"active_transactions\":%llu}",
                       entry->tx_id_hex,
                       (unsigned long long)active_now);
        if (len < 0) { rc = -1; goto abort_done; }
        /* Abort took the tx terminal — see the matching clear in COMMIT_TX
         * for why we do this. */
        if (tx_ctx && tx_ctx->has_tx &&
            memcmp(tx_ctx->tx_id, meta.tx_id, 16) == 0) {
            tx_ctx->has_tx = 0;
        }
        rc = send_frame(client_fd, FTX2_FRAME_ABORT_TX_ACK, 0, hdr.trace_id,
                        body, (uint64_t)len);
abort_done:
        runtime_release_tx_entry(state, entry);
        return rc;
    }

    /* ── TAKEOVER_REQUEST ── */
    if (hdr.frame_type == FTX2_FRAME_TAKEOVER_REQUEST) {
        state->takeover_requested = 1;
        runtime_mark_active_transactions(state, "interrupted");
        state->shutdown_requested = 1;
        (void)runtime_append_tx_event(state, "takeover_request");
        return send_frame(client_fd, FTX2_FRAME_TAKEOVER_ACK, 0, hdr.trace_id, "{}", 2);
    }

    /* ── SHUTDOWN ── */
    if (hdr.frame_type == FTX2_FRAME_SHUTDOWN) {
        runtime_mark_active_transactions(state, "interrupted");
        state->shutdown_requested = 1;
        (void)runtime_append_tx_event(state, "shutdown");
        return send_frame(client_fd, FTX2_FRAME_SHUTDOWN_ACK, 0, hdr.trace_id, "{}", 2);
    }

    /* ── CLEANUP ── */
    if (hdr.frame_type == FTX2_FRAME_CLEANUP) {
        return handle_cleanup(state, client_fd, hdr.trace_id,
                              request_body, hdr.body_len);
    }

    /* ── FS_LIST_VOLUMES ── */
    if (hdr.frame_type == FTX2_FRAME_FS_LIST_VOLUMES) {
        return handle_fs_list_volumes(state, client_fd, hdr.trace_id);
    }

    /* ── FS_LIST_DIR ── */
    if (hdr.frame_type == FTX2_FRAME_FS_LIST_DIR) {
        return handle_fs_list_dir(state, client_fd, hdr.trace_id,
                                   request_body, hdr.body_len);
    }

    /* ── FS_HASH ── */
    if (hdr.frame_type == FTX2_FRAME_FS_HASH) {
        return handle_fs_hash(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }

    /* ── FS destructive ops ── */
    if (hdr.frame_type == FTX2_FRAME_FS_DELETE) {
        return handle_fs_delete(state, client_fd, hdr.trace_id,
                                 request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MOVE) {
        return handle_fs_move(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_CHMOD) {
        return handle_fs_chmod(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MKDIR) {
        return handle_fs_mkdir(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_READ) {
        return handle_fs_read(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_COPY) {
        return handle_fs_copy(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_OP_STATUS) {
        return handle_fs_op_status(client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_OP_CANCEL) {
        return handle_fs_op_cancel(client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MOUNT) {
        return handle_fs_mount(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_UNMOUNT) {
        return handle_fs_unmount(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }

    /* ── App lifecycle (register / launch / list) ── */
    if (hdr.frame_type == FTX2_FRAME_APP_REGISTER) {
        return handle_app_register(state, client_fd, hdr.trace_id,
                                    request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_UNREGISTER) {
        return handle_app_unregister(state, client_fd, hdr.trace_id,
                                      request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LAUNCH) {
        return handle_app_launch(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LIST_REGISTERED) {
        return handle_app_list_registered(state, client_fd, hdr.trace_id,
                                           request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LAUNCH_BROWSER) {
        return handle_app_launch_browser(state, client_fd, hdr.trace_id);
    }

    /* ── Hardware monitoring ── */
    if (hdr.frame_type == FTX2_FRAME_HW_INFO) {
        return handle_hw_info(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_TEMPS) {
        return handle_hw_temps(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_POWER) {
        return handle_hw_power(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_STORAGE) {
        return handle_hw_storage(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_SET_FAN_THRESHOLD) {
        return handle_hw_set_fan_threshold(state, client_fd, hdr.trace_id,
                                            request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_TIME_GET) {
        return handle_time_get(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_TIME_SET) {
        return handle_time_set(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_TIME_STATE_GET) {
        return handle_time_state_get(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_TIME_STATE_SET) {
        return handle_time_state_set(state, client_fd, hdr.trace_id,
                                       request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_SMP_META_CONTROL) {
        return handle_smp_meta_control(state, client_fd, hdr.trace_id,
                                        request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_SMP_META_STATS) {
        return handle_smp_meta_stats(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_PROC_LIST) {
        return handle_proc_list(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_PROCESS_LIST) {
        return handle_process_list(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_PROCESS_KILL) {
        return handle_process_kill(state, client_fd, hdr.trace_id,
                                   request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_SYSLOG_TAIL) {
        return handle_syslog_tail(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_INFO) {
        return handle_profile_info(client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_SET_USERNAME) {
        return handle_profile_set_username(client_fd, hdr.trace_id,
                                           request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_ACTIVATE) {
        return handle_profile_activate(client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_APPLY_AVATAR) {
        return handle_profile_apply_avatar(client_fd, hdr.trace_id,
                                           request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_CLEAR_SLOT) {
        return handle_profile_clear_slot(client_fd, hdr.trace_id,
                                         request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_PROFILE_SET_LOCAL_USERNAME) {
        return handle_profile_set_local_username(client_fd, hdr.trace_id,
                                                 request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_PKG_INSTALL) {
        return handle_pkg_install(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_PKG_INSTALL_STATUS) {
        return handle_pkg_install_status(state, client_fd, hdr.trace_id,
                                         request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_SYSTEM_CONTROL) {
        return handle_system_control(state, client_fd, hdr.trace_id,
                                     request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_POWER_TELEMETRY) {
        return handle_power_telemetry(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_USER_LIST) {
        return handle_user_list(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_USER_CREATE) {
        return handle_user_create(state, client_fd, hdr.trace_id,
                                  request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_USER_DELETE) {
        return handle_user_delete(state, client_fd, hdr.trace_id,
                                  request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_BACKUP_SNAPSHOT) {
        return handle_backup_snapshot(state, client_fd, hdr.trace_id,
                                      request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_BACKUP_LIST) {
        return handle_backup_list(state, client_fd, hdr.trace_id,
                                  request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_BACKUP_RESTORE) {
        return handle_backup_restore(state, client_fd, hdr.trace_id,
                                     request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_BACKUP_DELETE) {
        return handle_backup_delete(state, client_fd, hdr.trace_id,
                                    request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_REMOTEPLAY_REQUEST) {
        return handle_remoteplay_request(state, client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_REMOTEPLAY_STATUS) {
        return handle_remoteplay_status(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_REMOTEPLAY_CANCEL) {
        return handle_remoteplay_cancel(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_FAN_CURVE_SET) {
        return handle_fan_curve_set(state, client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_NOTIF_LIST) {
        return handle_notif_list(state, client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_NOTIF_SEND) {
        return handle_notif_send(state, client_fd, hdr.trace_id, request_body);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_FAN_CURVE_GET) {
        return handle_fan_curve_get(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_LIST_SAVES) {
        return handle_list_saves(state, client_fd, hdr.trace_id,
                                 request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_LIST_SCREENSHOTS) {
        return handle_list_screenshots(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_LIST_VIDEOS) {
        return handle_list_videos(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_DRIVE_SENSORS) {
        return handle_hw_drive_sensors(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_INDEX_START) {
        return handle_index_start(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_INDEX_STATUS) {
        return handle_index_status(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_SEARCH_INDEX) {
        return handle_search_index(state, client_fd, hdr.trace_id,
                                   request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_INDEX_CANCEL) {
        return handle_index_cancel(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LIFECYCLE) {
        return handle_app_lifecycle(state, client_fd, hdr.trace_id,
                                    request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_KLOG_READ) {
        return handle_klog_read(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_NET_INTERFACES) {
        return handle_net_interfaces(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_PERIPHERAL_CONTROL) {
        return handle_peripheral_control(state, client_fd, hdr.trace_id,
                                          request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_PROC_MODULES) {
        return handle_proc_modules(state, client_fd, hdr.trace_id,
                                    request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_SHELL_EXEC) {
        return handle_shell_exec(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_CRC32_FILE) {
        return handle_crc32_file(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APPDB_QUERY) {
        return handle_appdb_query(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_NET_SPEED_TEST) {
        return handle_net_speed_test(state, client_fd, hdr.trace_id,
                                      request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_PKG_DIRECT_MOUNT) {
        return handle_pkg_direct_mount(state, client_fd, hdr.trace_id,
                                        request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_UFS_FSCK) {
        return handle_ufs_fsck(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_LWFS_MOUNT) {
        return handle_lwfs_mount(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                      "unsupported_frame", 17);
}

/* ── Listener cleanup (callable from signal handlers) ────────────────────────── */

void runtime_cleanup_listener(runtime_state_t *state) {
    if (!state) return;
    if (state->listener_fd >= 0) {
        close(state->listener_fd);
        state->listener_fd = -1;
    }
    if (state->mgmt_listener_fd >= 0) {
        close(state->mgmt_listener_fd);
        state->mgmt_listener_fd = -1;
    }
}

/* ── Server loop helpers ─────────────────────────────────────────────────────── */

/* Create, configure, bind, and listen on a TCP port. Returns the fd on
 * success, -1 on failure. `port` must be in network-byte-order untouched
 * (we htons it internally). Shared between the transfer and management
 * listeners so we don't duplicate the RCVBUF/SNDBUF probing dance. */
static int create_listener(int port, int probe_max_rcvbuf, int *out_asked,
                           int *out_actual_rcv, int *out_actual_snd,
                           int *out_probed_max) {
    struct sockaddr_in addr;
    int bind_attempt = 0;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("[payload2] socket");
        return -1;
    }
    {
        int one = 1;
        int rcvbuf = PS5UPLOAD2_CLIENT_RCVBUF_BYTES;
        int sndbuf = PS5UPLOAD2_CLIENT_SNDBUF_BYTES;
        int actual = 0;
        int rc_rcv, rc_snd;
        socklen_t actual_len = sizeof(actual);
        (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        rc_rcv = setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
        rc_snd = setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, sizeof(sndbuf));
        (void)rc_rcv; (void)rc_snd;
        if (out_asked) *out_asked = rcvbuf;
        if (getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &actual, &actual_len) == 0) {
            if (out_actual_rcv) *out_actual_rcv = actual;
        }
        if (getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &actual, &actual_len) == 0) {
            if (out_actual_snd) *out_actual_snd = actual;
        }
        if (probe_max_rcvbuf && out_probed_max) {
            int probes[] = {64*1024, 128*1024, 256*1024, 512*1024,
                            1024*1024, 2*1024*1024, 4*1024*1024,
                            8*1024*1024, 16*1024*1024};
            size_t i;
            int probe_fd = socket(AF_INET, SOCK_STREAM, 0);
            if (probe_fd >= 0) {
                int best = 0;
                for (i = 0; i < sizeof(probes)/sizeof(probes[0]); i++) {
                    int v = probes[i];
                    int got = 0;
                    socklen_t glen = sizeof(got);
                    (void)setsockopt(probe_fd, SOL_SOCKET, SO_RCVBUF, &v, sizeof(v));
                    if (getsockopt(probe_fd, SOL_SOCKET, SO_RCVBUF, &got, &glen) == 0) {
                        if (got > best) best = got;
                    }
                }
                close(probe_fd);
                *out_probed_max = best;
            }
        }
    }
    memset(&addr, 0, sizeof(addr));
    addr.sin_family      = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port        = htons((uint16_t)port);
    for (bind_attempt = 0; bind_attempt < 20; bind_attempt++) {
        if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0) break;
        if (errno != EADDRINUSE) {
            perror("[payload2] bind");
            close(fd);
            return -1;
        }
        usleep(100000);
    }
    if (bind_attempt >= 20) {
        fprintf(stderr, "[payload2] bind timeout on port %d\n", port);
        close(fd);
        return -1;
    }
    /* Generous accept backlog (was 8). Both the transfer (9113) and mgmt
     * (9114) listeners share this helper. A burst of concurrent connects —
     * e.g. a host firing several directory walks at once — could overflow a
     * backlog of 8, and the kernel then RSTs the excess so the host sees
     * "connect ... refused". The host side now serializes its remote walks,
     * but a larger backlog is cheap insurance against any transient burst
     * (FreeBSD clamps this to the kernel's somaxconn). */
    if (listen(fd, 128) != 0) {
        perror("[payload2] listen");
        close(fd);
        return -1;
    }
    return fd;
}

/* Per-connection socket tuning applied to every accepted fd. The PS5
 * kernel caps per-socket SO_RCVBUF at ~512 KiB, so we do the sizing on
 * the accepted socket rather than the listener. */
static void tune_accepted_client(int client_fd, int *out_last_rcvbuf) {
    int one = 1;
    int rcvbuf = PS5UPLOAD2_CLIENT_RCVBUF_BYTES;
    int sndbuf = PS5UPLOAD2_CLIENT_SNDBUF_BYTES;
    int actual_rcvbuf = 0;
    socklen_t actual_len = sizeof(actual_rcvbuf);
    struct timeval idle_to;
    idle_to.tv_sec  = PS5UPLOAD2_CLIENT_IDLE_SEC;
    idle_to.tv_usec = 0;
    (void)setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    (void)setsockopt(client_fd, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof(one));
    /* SO_RCVTIMEO is the knob that converts a stalled client into an
     * explicit recv error (EAGAIN) rather than an indefinite block on
     * recv(). If it fails silently, a hung client pins this worker
     * thread forever — no diagnostic, no recovery. Log any failure
     * loudly so the dev log carries the signal even if other settings
     * silently fall back. */
    if (setsockopt(client_fd, SOL_SOCKET, SO_RCVTIMEO, &idle_to, sizeof(idle_to)) != 0) {
        fprintf(stderr,
                "[payload] setsockopt(SO_RCVTIMEO=%lds) failed on fd=%d: %s (errno=%d) — "
                "a stalled client will now block this worker indefinitely\n",
                (long)idle_to.tv_sec, client_fd, strerror(errno), errno);
    }
    (void)setsockopt(client_fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
    (void)setsockopt(client_fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, sizeof(sndbuf));
    if (out_last_rcvbuf &&
        getsockopt(client_fd, SOL_SOCKET, SO_RCVBUF,
                   &actual_rcvbuf, &actual_len) == 0) {
        *out_last_rcvbuf = actual_rcvbuf;
    }
}

/* ── Transfer server loop (port 9113) ────────────────────────────────────────── */

/* Multi-stream support: the transfer port used to be strictly single-client
 * (accept → handle inline → close → accept next), which pinned upload throughput
 * to one stream's single-thread write speed (~40 MB/s on non-Pro PS5s). We now
 * dispatch each accepted connection to its own worker thread — mirroring the
 * mgmt port — so the engine can open several connections and upload disjoint
 * file sets concurrently. The tx table + per-file writes are already
 * concurrency-safe (per-slot g_entry_mtx; the pack pool writes 4 files at once),
 * so the only change needed is to stop serialising connections here.
 *
 * Backward compatible: an old single-stream engine opens exactly one connection
 * and behaves as before (just handled on a worker thread instead of the accept
 * thread). The number of streams the engine may open is advertised via STATUS
 * (`max_transfer_streams`); old engines that don't read it stay at one. */

/* Internal cap on concurrent transfer-handler threads. This is a safety
 * backstop — the engine is told a SMALLER number (STREAMS_ADVERTISED) and never
 * exceeds it — so the headroom here only absorbs the transient where a stream
 * drops and reconnects (RESUME) while its old connection's thread is still
 * draining. Kept well under any firmware's thread ceiling. */
#define PS5UPLOAD2_MAX_TRANSFER_THREADS 8
/* (PS5UPLOAD2_TRANSFER_STREAMS_ADVERTISED — the engine-facing limit — lives in
 * config.h since the STATUS handler above this point reports it. The thread cap
 * is 2x that for reconnect headroom.) */
static pthread_mutex_t g_transfer_count_mtx = PTHREAD_MUTEX_INITIALIZER;
static int g_transfer_thread_count = 0;

static void transfer_thread_count_inc(void) {
    pthread_mutex_lock(&g_transfer_count_mtx);
    g_transfer_thread_count += 1;
    pthread_mutex_unlock(&g_transfer_count_mtx);
}

static void transfer_thread_count_dec(void) {
    pthread_mutex_lock(&g_transfer_count_mtx);
    if (g_transfer_thread_count > 0) g_transfer_thread_count -= 1;
    pthread_mutex_unlock(&g_transfer_count_mtx);
}

/* pthread cleanup handler so the count is always decremented even if the
 * worker thread is cancelled or exits abnormally (e.g. crash inside
 * handle_binary_frame). The count is only a safety cap (8) above the
 * advertised limit (4); a leak just means we may fall back to inline
 * handling for one extra connection until the payload is replaced. */
static void transfer_thread_cleanup(void *arg) {
    (void)arg;
    transfer_thread_count_dec();
}

static int transfer_thread_can_spawn(void) {
    int can;
    pthread_mutex_lock(&g_transfer_count_mtx);
    can = (g_transfer_thread_count < PS5UPLOAD2_MAX_TRANSFER_THREADS);
    pthread_mutex_unlock(&g_transfer_count_mtx);
    return can;
}

typedef struct {
    runtime_state_t *state;
    int              client_fd;
} transfer_client_ctx_t;

/* Run one transfer connection to completion: handle frames until the client
 * closes it, an I/O error occurs, or shutdown is requested. Tracks the open tx
 * so a plain TCP disconnect (which runs neither COMMIT nor ABORT) marks the
 * journal entry "interrupted" instead of leaving an "active" orphan that a later
 * TX_FLAG_RESUME would miss. Shared by the threaded path and the inline backstop. */
static void transfer_handle_connection(runtime_state_t *state, int client_fd) {
    conn_tx_ctx_t tx_ctx = {0};
    while (!state->shutdown_requested) {
        if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/1, &tx_ctx) != 0) break;
    }
    if (tx_ctx.has_tx) {
        runtime_mark_tx_interrupted_by_id(state, tx_ctx.tx_id);
    }
    close(client_fd);
}

static void *transfer_client_thread(void *arg) {
    transfer_client_ctx_t *ctx = (transfer_client_ctx_t *)arg;
    runtime_state_t *state = ctx->state;
    int client_fd = ctx->client_fd;
    free(ctx);

    /* Guarantee dec even on pthread_cancel or crash paths inside the
     * connection handler. Normal return also runs the pop(1). */
    pthread_cleanup_push(transfer_thread_cleanup, NULL);
    transfer_handle_connection(state, client_fd);
    pthread_cleanup_pop(1);
    return NULL;
}

int runtime_server_loop(runtime_state_t *state) {
    if (!state) return -1;

    state->listener_fd = create_listener(state->runtime_port,
                                         /*probe_max_rcvbuf=*/1,
                                         &state->listener_rcvbuf_asked,
                                         &state->listener_rcvbuf_actual,
                                         &state->listener_sndbuf_actual,
                                         &state->max_rcvbuf_probed);
    if (state->listener_fd < 0) return -1;

    printf("[payload2] transfer listener ready on port %d\n", state->runtime_port);

    while (!state->shutdown_requested) {
        int client_fd = accept(state->listener_fd, NULL, NULL);
        if (client_fd < 0) {
            if (state->shutdown_requested) break; /* listener closed from outside */
            /* Transient per-connection errors must NOT tear down the whole
             * helper. Previously ANY non-EINTR error broke the loop and killed
             * the payload — a single ECONNABORTED or an fd-pressure spike under
             * a heavy Library scan dropped the helper, surfacing as "helper
             * keeps dying randomly". Retry instead. */
            if (errno == EINTR || errno == ECONNABORTED) continue;
            if (errno == EMFILE || errno == ENFILE || errno == ENOBUFS ||
                errno == ENOMEM) {
                perror("[payload2] accept (transient resource pressure)");
                usleep(100000); /* 100 ms back-off so we don't busy-spin */
                continue;
            }
            perror("[payload2] accept");
            break;
        }
        /* Tune on the accept thread (writes state->last_client_rcvbuf, which no
         * other thread touches) BEFORE handing off, so the worker threads never
         * race on it. */
        tune_accepted_client(client_fd, &state->last_client_rcvbuf);

        int use_thread = transfer_thread_can_spawn();
        if (use_thread) {
            transfer_client_ctx_t *ctx = (transfer_client_ctx_t *)malloc(sizeof(*ctx));
            if (!ctx) {
                fprintf(stderr, "[payload2] transfer: oom allocating client ctx; inline\n");
                use_thread = 0;
            } else {
                ctx->state = state;
                ctx->client_fd = client_fd;
                transfer_thread_count_inc();
                /* Detached-from-birth (see mgmt loop for the zombie rationale).
                 * Larger stack than mgmt: the transfer path runs the manifest
                 * parser + pack/direct writers. */
                pthread_attr_t attr;
                pthread_attr_t *attr_p = NULL;
                if (pthread_attr_init(&attr) == 0) {
                    if (pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0) {
                        attr_p = &attr;
                    }
                }
                if (attr_p) {
                    (void)pthread_attr_setstacksize(attr_p, 1024u * 1024u);
                }
                pthread_t tid;
                int rc = pthread_create(&tid, attr_p, transfer_client_thread, ctx);
                if (attr_p) pthread_attr_destroy(attr_p);
                if (rc != 0) {
                    /* Roll back and fall through to inline rather than drop. */
                    transfer_thread_count_dec();
                    free(ctx);
                    fprintf(stderr, "[payload2] transfer: pthread_create failed (%d); inline fallback\n", rc);
                    use_thread = 0;
                }
            }
        }

        if (!use_thread) {
            /* Backstop: handle this connection inline on the accept thread.
             * Blocks new accepts for this transfer's duration, but only reached
             * at the thread cap (which the engine never drives us to) or on
             * pthread_create failure — a rare, safe-degradation path. */
            transfer_handle_connection(state, client_fd);
        }
    }

    if (state->listener_fd >= 0) {
        close(state->listener_fd);
        state->listener_fd = -1;
    }
    printf("[payload2] transfer loop exiting shutdown=%d takeover=%d commands=%llu\n",
           state->shutdown_requested,
           state->takeover_requested,
           (unsigned long long)state->command_count);
    return 0;
}

/* ── Management server loop (pthread, port 9114) ─────────────────────────────── */

/* Per-client handler spawned from the mgmt accept loop. Running each
 * client on its own pthread is critical: the mgmt port services
 * FS_LIST_DIR / FS_HASH / FS_READ which call into the kernel's VFS,
 * and those syscalls can legitimately block indefinitely on a stuck
 * USB mount, a dead NAS, or a filesystem in an error state. If the
 * accept loop stayed single-threaded, one stuck readdir would wedge
 * the entire mgmt port -- meaning TAKEOVER_REQUEST from a newly-sent
 * payload never gets handled and the user has to network-reset to
 * kill the zombie. Per-thread isolation contains the stall to the
 * one client that triggered it.
 *
 * The thread arg is a heap-allocated { state, client_fd } tuple; the
 * handler frees it on exit. Threads are detached so no joinable state
 * leaks during normal churn. */
typedef struct {
    runtime_state_t *state;
    int              client_fd;
} mgmt_client_ctx_t;

/* Cap on concurrent mgmt-handler threads. Without a cap, a deep
 * Library scan (thousands of FS_LIST_DIR calls over a populated PS5
 * filesystem) spawns enough detached pthreads that the payload
 * eventually exhausts PS5's thread/memory budget and crashes. With a
 * cap, excess clients fall through to inline handling in the accept
 * thread -- slower but never resource-exhausting. 8 is enough to
 * hide a few slow syscalls without wedging the whole mgmt port, and
 * well under any firmware's thread ceiling. */
#define PS5UPLOAD2_MAX_MGMT_THREADS 8
static pthread_mutex_t g_mgmt_count_mtx = PTHREAD_MUTEX_INITIALIZER;
static int g_mgmt_thread_count = 0;

static void mgmt_thread_count_inc(void) {
    pthread_mutex_lock(&g_mgmt_count_mtx);
    g_mgmt_thread_count += 1;
    pthread_mutex_unlock(&g_mgmt_count_mtx);
}

static void mgmt_thread_count_dec(void) {
    pthread_mutex_lock(&g_mgmt_count_mtx);
    if (g_mgmt_thread_count > 0) g_mgmt_thread_count -= 1;
    pthread_mutex_unlock(&g_mgmt_count_mtx);
}

static int mgmt_thread_can_spawn(void) {
    int can;
    pthread_mutex_lock(&g_mgmt_count_mtx);
    can = (g_mgmt_thread_count < PS5UPLOAD2_MAX_MGMT_THREADS);
    pthread_mutex_unlock(&g_mgmt_count_mtx);
    return can;
}

static void *mgmt_client_thread(void *arg) {
    mgmt_client_ctx_t *ctx = (mgmt_client_ctx_t *)arg;
    runtime_state_t *state = ctx->state;
    int client_fd = ctx->client_fd;
    free(ctx);

    while (state && !state->shutdown_requested) {
        /* Mgmt port handles no transfer frames; no tx context needed. */
        if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/0, NULL) != 0) break;
    }
    close(client_fd);
    mgmt_thread_count_dec();
    return NULL;
}

void *runtime_mgmt_server_loop(void *state_ptr) {
    runtime_state_t *state = (runtime_state_t *)state_ptr;
    if (!state) return NULL;

    state->mgmt_listener_fd = create_listener(state->mgmt_port,
                                              /*probe_max_rcvbuf=*/0,
                                              NULL, NULL, NULL, NULL);
    if (state->mgmt_listener_fd < 0) {
        fprintf(stderr, "[payload2] mgmt listener failed on port %d\n",
                state->mgmt_port);
        return NULL;
    }

    printf("[payload2] mgmt listener ready on port %d\n", state->mgmt_port);

    while (!state->shutdown_requested) {
        int client_fd = accept(state->mgmt_listener_fd, NULL, NULL);
        if (client_fd < 0) {
            /* When main() signals shutdown it closes mgmt_listener_fd from
             * the outside; accept() returns with EBADF and we exit cleanly. */
            if (state->shutdown_requested) break;
            /* Keep the MGMT listener alive across transient errors — it's the
             * port the host's liveness probe hits, so breaking here makes the
             * app declare "Helper isn't running" even though the helper is up. */
            if (errno == EINTR || errno == ECONNABORTED) continue;
            if (errno == EMFILE || errno == ENFILE || errno == ENOBUFS ||
                errno == ENOMEM) {
                perror("[payload2] mgmt accept (transient resource pressure)");
                usleep(100000); /* 100 ms back-off */
                continue;
            }
            perror("[payload2] mgmt accept");
            break;
        }
        tune_accepted_client(client_fd, NULL);

        /* Hand the client off to its own thread so a stuck fs syscall
         * can't block the accept loop -- BUT cap the concurrent
         * thread count. A deep Library scan walks thousands of
         * directories; without the cap, we spawn one pthread per
         * connection and eventually exhaust PS5's thread/memory
         * budget (verified: 15k requests crashed the payload). When
         * the cap is reached, we fall back to inline handling in
         * the accept thread -- sequential but stable. */
        int use_thread = mgmt_thread_can_spawn();

        if (use_thread) {
            mgmt_client_ctx_t *ctx = (mgmt_client_ctx_t *)malloc(sizeof(*ctx));
            if (!ctx) {
                fprintf(stderr, "[payload2] mgmt: oom allocating client ctx; closing\n");
                close(client_fd);
                continue;
            }
            ctx->state = state;
            ctx->client_fd = client_fd;
            mgmt_thread_count_inc();
            /* Detached-from-birth. Previous code was pthread_create + late
             * pthread_detach, which has a window where a fast-exiting
             * thread becomes a zombie between create and detach. After
             * many zombies accumulate, the kernel exhausts thread slots
             * and every subsequent handler hangs. This was observed in
             * stress tests where trivial (validation-only) handlers
             * hung after a few calls because they exited too fast for
             * the late detach to matter. */
            pthread_attr_t attr;
            pthread_attr_t *attr_p = NULL;
            if (pthread_attr_init(&attr) == 0) {
                if (pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0) {
                    attr_p = &attr;
                }
            }
            pthread_t tid;
            if (attr_p) {
                (void)pthread_attr_setstacksize(attr_p, 512u * 1024u);
            }
            int rc = pthread_create(&tid, attr_p, mgmt_client_thread, ctx);
            if (attr_p) pthread_attr_destroy(attr_p);
            if (rc == 0) {
                /* Already detached; no pthread_detach call needed. */
            } else {
                /* Thread creation failed despite our cap allowing it
                 * (kernel refused for other reasons, e.g., EAGAIN).
                 * Roll back our counter and fall through to inline
                 * handling rather than drop the client. */
                mgmt_thread_count_dec();
                free(ctx);
                fprintf(stderr, "[payload2] mgmt: pthread_create failed (%d); inline fallback\n", rc);
                use_thread = 0;
            }
        }

        if (!use_thread) {
            /* Inline fallback: process this client's frames directly
             * in the accept thread. Slower for burst traffic but
             * guarantees we don't exhaust thread resources during a
             * deep Library scan. */
            while (!state->shutdown_requested) {
                if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/0, NULL) != 0) break;
            }
            close(client_fd);
        }
    }

    if (state->mgmt_listener_fd >= 0) {
        close(state->mgmt_listener_fd);
        state->mgmt_listener_fd = -1;
    }
    printf("[payload2] mgmt loop exiting shutdown=%d\n", state->shutdown_requested);
    return NULL;
}
