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
#include <poll.h>
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
#include "sandbox_unmount.h"
#include "instance_verdict.h"

#include "content_db.h"
#include "register.h"
#include "hw_info.h"
#include "drive_sensors.h"
#include "backup.h"
#include "fs_jobs.h"
#include "remoteplay.h"
#include "fan_curve.h"
#include "notif.h"
#include "cheats.h"
#include "activity.h"
#include "sdk_changer.h"
#include "tmdb.h"
#include "fw_spoof.h"
#include "ftp_server.h"
#include "sys_time.h"
#include "sys_registry.h"
#include "profile.h"
#include "sony_api_lock.h"
#include "net_probe.h"
#include "focus_probe.h"
#include "proc_list.h"
#include "proc_identity.h"
#include "smp_meta.h"
#include "blake3.h"
#include "mgmt_rpc.h"
#include "mgmt_fs.h"
#include "path_policy.h"
#include "cross_device.h"
#include "ava1_stop.h"
#include "ava1_glue.h"
#include "ownership_record.h"

/* Capacity policy mirrored by ps5upload-core::volumes. Keep a small working
 * margin -- 1 GiB, scaled down for small volumes -- out of every filesystem
 * for metadata and concurrent activity. The PS5 user-storage allocator can
 * still return ENOSPC while statfs(/data) advertises far more; that is not
 * predictable here and is handled after the fact, not by this gate. See
 * capacity_reserve_for_mount. */
#define PS5UPLOAD2_GIB ((uint64_t)1024u * 1024u * 1024u)
#define PS5UPLOAD2_EXTERNAL_SPACE_RESERVE (1u * PS5UPLOAD2_GIB)

/* The frame numbers the management table (mgmt_table.def) carries per method. They are not on any wire now:
 * the number a handler runs under is written to g_inflight_frame_type, so the fatal-signal breadcrumb in
 * main.c ("while serving frame N") still names the operation, and the ACK/ERROR numbers name what a handler
 * answers with through mgmt_reply (mgmt_rpc.c turns any non-ERROR frame into an OK reply). Keep the values:
 * old stderr.log files and bug reports name an operation by its number. */
#define MGMT_FRAME_STATUS 20u
#define MGMT_FRAME_STATUS_ACK 21u
#define MGMT_FRAME_SHUTDOWN 22u
#define MGMT_FRAME_SHUTDOWN_ACK 23u
#define MGMT_FRAME_CLEANUP 32u
#define MGMT_FRAME_CLEANUP_ACK 33u
#define MGMT_FRAME_FS_LIST_VOLUMES 34u
#define MGMT_FRAME_FS_LIST_VOLUMES_ACK 35u
#define MGMT_FRAME_FS_LIST_DIR 36u
#define MGMT_FRAME_FS_LIST_DIR_ACK 37u
#define MGMT_FRAME_FS_MOVE 42u
#define MGMT_FRAME_FS_MOVE_ACK 43u
#define MGMT_FRAME_FS_CHMOD 44u
#define MGMT_FRAME_FS_CHMOD_ACK 45u
#define MGMT_FRAME_FS_MKDIR 46u
#define MGMT_FRAME_FS_MKDIR_ACK 47u
#define MGMT_FRAME_FS_READ 48u
#define MGMT_FRAME_FS_READ_ACK 49u
#define MGMT_FRAME_FS_MOUNT 52u
#define MGMT_FRAME_FS_MOUNT_ACK 53u
#define MGMT_FRAME_FS_UNMOUNT 54u
#define MGMT_FRAME_FS_UNMOUNT_ACK 55u
#define MGMT_FRAME_APP_REGISTER 56u
#define MGMT_FRAME_APP_REGISTER_ACK 57u
#define MGMT_FRAME_APP_UNREGISTER 58u
#define MGMT_FRAME_APP_UNREGISTER_ACK 59u
#define MGMT_FRAME_APP_LAUNCH 60u
#define MGMT_FRAME_APP_LAUNCH_ACK 61u
#define MGMT_FRAME_APP_LIST_REGISTERED 62u
#define MGMT_FRAME_APP_LIST_REGISTERED_ACK 63u
#define MGMT_FRAME_HW_INFO 64u
#define MGMT_FRAME_HW_INFO_ACK 65u
#define MGMT_FRAME_HW_TEMPS 66u
#define MGMT_FRAME_HW_TEMPS_ACK 67u
#define MGMT_FRAME_HW_POWER 68u
#define MGMT_FRAME_HW_POWER_ACK 69u
#define MGMT_FRAME_APP_LAUNCH_BROWSER 70u
#define MGMT_FRAME_APP_LAUNCH_BROWSER_ACK 71u
#define MGMT_FRAME_HW_SET_FAN_THRESHOLD 72u
#define MGMT_FRAME_HW_SET_FAN_THRESHOLD_ACK 73u
#define MGMT_FRAME_HW_STORAGE 80u
#define MGMT_FRAME_HW_STORAGE_ACK 81u
#define MGMT_FRAME_PROC_LIST 74u
#define MGMT_FRAME_PROC_LIST_ACK 75u
#define MGMT_FRAME_SYSTEM_CONTROL 86u
#define MGMT_FRAME_SYSTEM_CONTROL_ACK 87u
#define MGMT_FRAME_POWER_TELEMETRY 88u
#define MGMT_FRAME_POWER_TELEMETRY_ACK 89u
#define MGMT_FRAME_USER_LIST 90u
#define MGMT_FRAME_USER_LIST_ACK 91u
#define MGMT_FRAME_LIST_SAVES 92u
#define MGMT_FRAME_LIST_SAVES_ACK 93u
#define MGMT_FRAME_LIST_SCREENSHOTS 94u
#define MGMT_FRAME_LIST_SCREENSHOTS_ACK 95u
#define MGMT_FRAME_INDEX_START 96u
#define MGMT_FRAME_INDEX_START_ACK 97u
#define MGMT_FRAME_INDEX_STATUS 98u
#define MGMT_FRAME_INDEX_STATUS_ACK 99u
#define MGMT_FRAME_SEARCH_INDEX 100u
#define MGMT_FRAME_SEARCH_INDEX_ACK 101u
#define MGMT_FRAME_INDEX_CANCEL 102u
#define MGMT_FRAME_INDEX_CANCEL_ACK 103u
#define MGMT_FRAME_APP_LIFECYCLE 104u
#define MGMT_FRAME_APP_LIFECYCLE_ACK 105u
#define MGMT_FRAME_TOAST_SEND 106u
#define MGMT_FRAME_TOAST_SEND_ACK 107u
#define MGMT_FRAME_KLOG_READ 108u
#define MGMT_FRAME_KLOG_READ_ACK 109u
#define MGMT_FRAME_NET_INTERFACES 110u
#define MGMT_FRAME_NET_INTERFACES_ACK 111u
#define MGMT_FRAME_PERIPHERAL_CONTROL 112u
#define MGMT_FRAME_PERIPHERAL_CONTROL_ACK 113u
#define MGMT_FRAME_PROC_MODULES 114u
#define MGMT_FRAME_PROC_MODULES_ACK 115u
#define MGMT_FRAME_SHELL_EXEC 116u
#define MGMT_FRAME_SHELL_EXEC_ACK 117u
#define MGMT_FRAME_APPDB_QUERY 120u
#define MGMT_FRAME_APPDB_QUERY_ACK 121u
#define MGMT_FRAME_NET_SPEED_TEST 122u
#define MGMT_FRAME_NET_SPEED_TEST_ACK 123u
#define MGMT_FRAME_NET_REACH 148u
#define MGMT_FRAME_NET_REACH_ACK 149u
#define MGMT_FRAME_PKG_DIRECT_MOUNT 124u
#define MGMT_FRAME_PKG_DIRECT_MOUNT_ACK 125u
#define MGMT_FRAME_UFS_FSCK 126u
#define MGMT_FRAME_UFS_FSCK_ACK 127u
#define MGMT_FRAME_LWFS_MOUNT 128u
#define MGMT_FRAME_LWFS_MOUNT_ACK 129u
#define MGMT_FRAME_FS_WRITE_BYTES 130u
#define MGMT_FRAME_FS_WRITE_BYTES_ACK 131u
#define MGMT_FRAME_TIME_GET 132u
#define MGMT_FRAME_TIME_GET_ACK 133u
#define MGMT_FRAME_TIME_SET 134u
#define MGMT_FRAME_TIME_SET_ACK 135u
#define MGMT_FRAME_TIME_STATE_GET 136u
#define MGMT_FRAME_TIME_STATE_GET_ACK 137u
#define MGMT_FRAME_TIME_STATE_SET 138u
#define MGMT_FRAME_TIME_STATE_SET_ACK 139u
#define MGMT_FRAME_SMP_META_CONTROL 140u
#define MGMT_FRAME_SMP_META_CONTROL_ACK 141u
#define MGMT_FRAME_SMP_META_STATS 142u
#define MGMT_FRAME_SMP_META_STATS_ACK 143u
#define MGMT_FRAME_SYSLOG_TAIL 144u
#define MGMT_FRAME_SYSLOG_TAIL_ACK 145u
#define MGMT_FRAME_PROFILE_INFO 150u
#define MGMT_FRAME_PROFILE_INFO_ACK 151u
#define MGMT_FRAME_PROFILE_SET_USERNAME 152u
#define MGMT_FRAME_PROFILE_SET_USERNAME_ACK 153u
#define MGMT_FRAME_PROFILE_ACTIVATE 154u
#define MGMT_FRAME_PROFILE_ACTIVATE_ACK 155u
#define MGMT_FRAME_PROFILE_APPLY_AVATAR 156u
#define MGMT_FRAME_PROFILE_APPLY_AVATAR_ACK 157u
#define MGMT_FRAME_PROFILE_CLEAR_SLOT 158u
#define MGMT_FRAME_PROFILE_CLEAR_SLOT_ACK 159u
#define MGMT_FRAME_PROFILE_SET_LOCAL_USERNAME 160u
#define MGMT_FRAME_PROFILE_SET_LOCAL_USERNAME_ACK 161u
#define MGMT_FRAME_PROCESS_LIST 162u
#define MGMT_FRAME_PROCESS_LIST_ACK 163u
#define MGMT_FRAME_PROCESS_KILL 164u
#define MGMT_FRAME_PROCESS_KILL_ACK 165u
#define MGMT_FRAME_LIST_VIDEOS 166u
#define MGMT_FRAME_LIST_VIDEOS_ACK 167u
#define MGMT_FRAME_HW_DRIVE_SENSORS 168u
#define MGMT_FRAME_HW_DRIVE_SENSORS_ACK 169u
#define MGMT_FRAME_USER_CREATE 170u
#define MGMT_FRAME_USER_CREATE_ACK 171u
#define MGMT_FRAME_USER_DELETE 172u
#define MGMT_FRAME_USER_DELETE_ACK 173u
#define MGMT_FRAME_BACKUP_SNAPSHOT 176u
#define MGMT_FRAME_BACKUP_SNAPSHOT_ACK 177u
#define MGMT_FRAME_BACKUP_LIST 178u
#define MGMT_FRAME_BACKUP_LIST_ACK 179u
#define MGMT_FRAME_BACKUP_RESTORE 180u
#define MGMT_FRAME_BACKUP_RESTORE_ACK 181u
#define MGMT_FRAME_BACKUP_DELETE 182u
#define MGMT_FRAME_BACKUP_DELETE_ACK 183u
#define MGMT_FRAME_REMOTEPLAY_REQUEST 188u
#define MGMT_FRAME_REMOTEPLAY_STATUS 189u
#define MGMT_FRAME_REMOTEPLAY_CANCEL 190u
#define MGMT_FRAME_REMOTEPLAY_CANCEL_ACK 191u
#define MGMT_FRAME_REMOTEPLAY_READINESS 248u
#define MGMT_FRAME_REMOTEPLAY_ENABLE 249u
#define MGMT_FRAME_REMOTEPLAY_DEVICES 250u
#define MGMT_FRAME_HW_FAN_CURVE_SET 196u
#define MGMT_FRAME_HW_FAN_CURVE_SET_ACK 197u
#define MGMT_FRAME_HW_FAN_CURVE_GET 246u
#define MGMT_FRAME_HW_FAN_CURVE_GET_ACK 247u
#define MGMT_FRAME_NOTIF_LIST 198u
#define MGMT_FRAME_NOTIF_LIST_ACK 199u
#define MGMT_FRAME_NOTIF_SEND 240u
#define MGMT_FRAME_NOTIF_SEND_ACK 241u
#define MGMT_FRAME_NOTIF_CLEAR 251u
#define MGMT_FRAME_NOTIF_CLEAR_ACK 252u
#define MGMT_FRAME_CHEATS_LIST 200u
#define MGMT_FRAME_CHEATS_LIST_ACK 201u
#define MGMT_FRAME_CHEATS_GET 202u
#define MGMT_FRAME_CHEATS_GET_ACK 203u
#define MGMT_FRAME_CHEATS_TOGGLE 204u
#define MGMT_FRAME_CHEATS_TOGGLE_ACK 205u
#define MGMT_FRAME_CHEATS_DELETE 206u
#define MGMT_FRAME_CHEATS_DELETE_ACK 207u
#define MGMT_FRAME_CHEATS_RELOAD 208u
#define MGMT_FRAME_CHEATS_RELOAD_ACK 209u
#define MGMT_FRAME_CHEATS_STATUS 210u
#define MGMT_FRAME_CHEATS_STATUS_ACK 211u
#define MGMT_FRAME_CHEATS_ENGINE_SET 212u
#define MGMT_FRAME_CHEATS_ENGINE_SET_ACK 213u
#define MGMT_FRAME_ACTIVITY_GET 192u
#define MGMT_FRAME_ACTIVITY_GET_ACK 193u
#define MGMT_FRAME_ACTIVITY_DB_QUERY 194u
#define MGMT_FRAME_ACTIVITY_DB_QUERY_ACK 195u
#define MGMT_FRAME_ACTIVITY_RESET 253u
#define MGMT_FRAME_ACTIVITY_RESET_ACK 254u
#define MGMT_FRAME_SDK_SCAN 214u
#define MGMT_FRAME_SDK_SCAN_ACK 215u
#define MGMT_FRAME_SDK_PATCH 216u
#define MGMT_FRAME_SDK_PATCH_ACK 217u
#define MGMT_FRAME_SDK_RESTORE 218u
#define MGMT_FRAME_SDK_RESTORE_ACK 219u
#define MGMT_FRAME_TMDB_FETCH 222u
#define MGMT_FRAME_TMDB_FETCH_ACK 223u
#define MGMT_FRAME_TMDB_STORE 228u
#define MGMT_FRAME_TMDB_STORE_ACK 229u
#define MGMT_FRAME_FTP_START 224u
#define MGMT_FRAME_FTP_START_ACK 225u
#define MGMT_FRAME_FTP_STATUS 226u
#define MGMT_FRAME_FTP_STATUS_ACK 227u
#define MGMT_FRAME_FWSPOOF_STATUS 232u
#define MGMT_FRAME_FWSPOOF_STATUS_ACK 233u
#define MGMT_FRAME_APPINFO_QUERY 234u
#define MGMT_FRAME_APPINFO_QUERY_ACK 235u
#define MGMT_FRAME_APPINFO_SET 236u
#define MGMT_FRAME_APPINFO_SET_ACK 237u
#define MGMT_FRAME_FOCUS_PROBE 174u
#define MGMT_FRAME_FOCUS_PROBE_ACK 175u
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

/* Answers the management call running on this thread. The AVA1 dispatcher (mgmt_rpc.c) runs every handler
 * behind a capture sink: the frame a handler "sends" is recorded there and becomes the RpcResponse (an
 * ERROR frame, or a body that says {"ok":false,...}, becomes an AVA1 error status). Returns 0, or -1
 * when the reply did not fit or no call is running. */
static int mgmt_reply(uint16_t frame_type, const void *body, uint64_t body_len) {
    return mgmt_capture_frame(frame_type, body, body_len);
}


/* Loop until `len` bytes have been written (EINTR retried). 0 on success, -1 on a hard error. */
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

/* Creates a joinable worker thread with a bounded 512 KiB stack (the FreeBSD default is several MiB and the
 * console's per-process memory budget is small). Falls back to the default stack if attribute setup fails. */
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

/* Component-scoped `..`/`.` rejection used by both is_path_allowed and
 * cleanup_path_allowed. Defined further down near is_path_allowed. */
static int path_has_dotdot_component(const char *p);
/* Writable-roots allowlist. Used by every destructive FS handler and
 * the AVA1 file operations. See is_path_allowed for the exact set. Also used by
 * backup.c's restore path so a crafted manifest can't write outside the
 * allowlist. Declared in runtime.h. */
int is_path_allowed(const char *p);
int is_safe_unsafe_read_path(const char *p);
/* JSON-escape helper. Used by ACK builders that embed user-controlled
 * paths/strings into JSON bodies. Defined alongside FS_LIST_VOLUMES. */
static size_t json_escape_into(const char *src, char *dst, size_t dst_cap);
static const char *json_string_end(const char *start, const char *limit);
static int json_copy_unescaped_string(const char *start, const char *end,
                                      char *out, size_t out_len);
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



/* ── Directory helper ───────────────────────────────────────────────────────── */

static int ensure_dir(const char *path) {
    if (!path || !*path) return -1;
    if (mkdir(path, 0777) == 0) return 0;
    if (errno == EEXIST) return 0;
    return -1;
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
    /* Match `"field":` and then skip whitespace before the opening quote.
     *
     * The old needle spliced the quote on (`"field":"`), so a body with a
     * space after the colon — which any pretty-printer emits, and which
     * is perfectly legal JSON — silently produced an EMPTY field instead
     * of an error. Callers then acted on "" as though the client had sent
     * it. Our engine happens to emit compact JSON, which is the only
     * reason this never bit us. */
    snprintf(needle, sizeof(needle), "\"%s\":", field);
    pos = strstr(json, needle);
    if (!pos) return;
    pos += strlen(needle);
    while (*pos == ' ' || *pos == '\t' || *pos == '\n' || *pos == '\r') pos++;
    if (*pos != '"') return; /* present but not a string value */
    start = pos + 1;
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

static int extract_json_bool_field(const char *json, const char *field,
                                   int *out) {
    char needle[64];
    const char *pos = NULL;
    if (!json || !field || !out) return -1;
    snprintf(needle, sizeof(needle), "\"%s\":", field);
    pos = strstr(json, needle);
    if (!pos) return -1;
    pos += strlen(needle);
    while (*pos == ' ' || *pos == '\t') pos++;
    if (strncmp(pos, "true", 4) == 0) { *out = 1; return 0; }
    if (strncmp(pos, "false", 5) == 0) { *out = 0; return 0; }
    return -1;
}

/* ── Public lifecycle ─────────────────────────────────────────────────────────── */

int runtime_ensure_directories(void) {
    /* Critical dirs — all under /data which the loader's process
     * always has write access to. If any of these fail, the payload
     * truly can't function, so abort startup. */
    if (ensure_dir(PS5UPLOAD2_RUNTIME_ROOT) != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_RUNTIME_DIR)  != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_DEBUG_DIR)    != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_MOUNTS_DIR)   != 0) return -1;
    /* Optional dirs under /user — Sony-managed root with stricter
     * permissions. Without ucred elevation (kstuff not loaded yet)
     * these mkdirs fail with EACCES. They're only used by the pkg
     * install flow, so a failure here MUST NOT abort startup —
     * otherwise sending ps5upload before kstuff would prevent the
     * server from ever starting, breaking the whole "load kstuff
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
 * may realloc, invalidating an earlier caller's pointer). AVA1 runs up to
 * eight management calls at once, several of
 * which call getmntinfo (FS_LIST_VOLUMES, the FS_MOUNT reuse scan, FS_UNMOUNT,
 * and the shell `mount`/`df`/`mtrw` verbs) — so two racing callers can read a
 * half-replaced array or a dangling pointer. Serialize the call and hand back
 * a PRIVATE heap copy the caller frees with free(). The lock is held only
 * around getmntinfo + the memcpy — a tiny, return-free region — never during
 * the caller's iteration, so it can neither deadlock nor serialize the stat()
 * work the callers do. Returns the entry count (0 on failure/empty) and sets
 * *out to a malloc'd array (NULL on failure/empty). EVERY caller must free()
 * the array on every exit path once it's done reading it. */
int mntinfo_snapshot(struct statfs **out) {
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

static uint64_t runtime_system_boottime_unix(void);

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
        if (clock_gettime(CLOCK_REALTIME, &rts) != 0 || rts.tv_sec <= 0) {
            rts.tv_sec = time(NULL);
            rts.tv_nsec = 0;
        }
        /* started_at_unix == 0 reads as "unverifiable" to the next instance's reap. A clock that
         * cannot be read still has a true lower bound: this boot's start. */
        if (rts.tv_sec <= 0) rts.tv_sec = (time_t)runtime_system_boottime_unix();
        if (rts.tv_sec <= 0) {
            fprintf(stderr, "[payload2] runtime_init: CLOCK_REALTIME, time() and kern.boottime all failed; "
                            "cannot stamp started_at_unix, so the ownership record could not identify this instance. "
                            "Refusing to start.\n");
            return -1;
        }
        uint64_t hi = ((uint64_t)rts.tv_sec & 0xFFFFFFFFu) << 32;
        uint64_t lo = ((uint64_t)getpid() << 16) ^ ((uint64_t)rts.tv_nsec & 0xFFFFu);
        state->instance_id = hi | (lo & 0xFFFFFFFFu);
        state->started_at_unix = (uint64_t)rts.tv_sec;
    }
    state->startup_reason   = PS5UPLOAD2_STARTUP_FRESH;
    /* Guards the counters the management handlers and node.status read from different threads. */
    if (pthread_mutex_init(&state->state_mtx, NULL) != 0) {
        fprintf(stderr, "[payload2] pthread_mutex_init failed\n");
        return -1;
    }
    snprintf(state->ownership_path, sizeof(state->ownership_path),
             "%s/active_instance.txt", PS5UPLOAD2_RUNTIME_DIR);
    backup_init();
    remoteplay_init();
    notif_init();
    cheats_init();
    activity_init();
    sdk_changer_init();
    tmdb_init();
    fw_spoof_init();
    ftp_server_init();
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
    /* Never publish a record with started_at_unix == 0 (the 2026-10-03 outage's unverifiable prior):
     * stamp it now if init could not. */
    uint64_t started = state->started_at_unix;
    if (started == 0) {
        time_t t = time(NULL);
        started = t > 0 ? (uint64_t)t : runtime_system_boottime_unix();
    }
    char rec[256];
    int rec_len = ownership_record_format(rec, sizeof rec, state->instance_id, 0 /* runtime_port: none now */,
                                          state->startup_reason, started, (int)getpid());
    if (rec_len < 0) {
        fprintf(stderr, "[payload2] refusing to write an ownership record without a start time\n");
        return -1;
    }
    fp = fopen(tmp_path, "w");
    if (!fp) {
        fprintf(stderr, "[payload2] failed to open ownership tmp %s\n", tmp_path);
        return -1;
    }
    fwrite(rec, 1, (size_t)rec_len, fp);
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
    printf("[payload2] ownership record instance=%llu path=%s\n",
           (unsigned long long)state->instance_id,
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

void runtime_classify_prior_instance(runtime_state_t *state) {
    if (!state) return;

    struct stat st;
    int record_present = (stat(state->ownership_path, &st) == 0) ? 1 : 0;
    uint64_t prior_started = 0;
    int prior_alive = 0;

    if (record_present) {
        /* One read of the whole record (retried once), kept on the state: by the time the reap
         * runs, the handing-over predecessor may have unlinked the file (review 010). */
        ownership_rec_t rec;
        (void)ownership_record_read(state->ownership_path, &rec, 20);
        state->prior_rec_pid = rec.pid;
        state->prior_rec_started = rec.started;
        prior_started = rec.started;
        int prior_pid = rec.pid;
        if (prior_pid > 0 && prior_pid != (int)getpid()) {
            prior_alive = (kill((pid_t)prior_pid, 0) == 0) ? 1 : 0;
        }
    }

    ps5upload2_prior_verdict_t v =
        instance_verdict_classify(record_present, prior_started,
                                  runtime_system_boottime_unix(), prior_alive);
    state->prior_verdict = (int)v;

    fprintf(stderr, "[payload2] prior instance: %s\n", instance_verdict_name(v));
    if (v == PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY) {
        fprintf(stderr,
                "[payload2] the previous instance was killed by something else on this "
                "console (SIGKILL or OOM) — it did not exit on its own\n");
    }
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
/* How long the prior instance gets to exit by itself before it is killed. */
#define REAP_GRACE_MS 15000

void runtime_reap_prior_instance(runtime_state_t *state) {
    if (!state) return;
    /* Read the whole record once, retried once, and fill what a lost read dropped from the snapshot
     * taken before the takeover. The predecessor unlinks the record as it hands over; reading pid
     * and start time in two separate opens let it vanish between them and read as started=0. */
    ownership_rec_t rec;
    (void)ownership_record_read(state->ownership_path, &rec, 20);
    {
        ownership_rec_t snap = {state->prior_rec_pid, state->prior_rec_started, 0};
        ownership_record_merge(&rec, &snap);
    }
    int prior = rec.pid > 0 ? rec.pid : -1;
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

    /* Boot-session guard (the critical safety gate). The ownership file lives on persistent /data and
     * SURVIVES reboots, and after a reboot the pid counter restarts, so a stale `pid=` may now be
     * unrelated homebrew (a cheat loader, nanoDNS, ...). Two witnesses, either enough: the KERNEL's
     * start time of that very process (ki_start, same clock as kern.boottime) is at or after this
     * boot, or the record's started_at_unix is. The record alone used to be the only one, and a
     * record that read started=0 left a live helper beside the new instance (the 2026-10-03 Pro
     * outage). The name check (ours, not "payload.elf") stays the second line of defence. */
    if (kill((pid_t)prior, 0) != 0) return; /* already gone */

    char their_name[64] = {0};
    if (proc_name_by_pid(prior, their_name, sizeof(their_name)) != 0) {
        return; /* prior pid vanished between the checks — nothing to do */
    }
    {
        uint64_t prior_started = rec.started;
        uint64_t boottime = runtime_system_boottime_unix();
        uint64_t kstart = 0;
        int kstart_known = proc_start_by_pid(prior, &kstart) == 0;
        ps5upload2_reap_t verdict = instance_reap_decision(proc_name_is_ours(their_name), prior_started, boottime,
                                                           kstart, kstart_known, (uint64_t)time(NULL));
        fprintf(stderr,
                "[payload2] reap: pid %d name=%s kernel_start=%llu%s record_started=%llu boottime=%llu -> %s\n",
                prior, their_name, (unsigned long long)kstart, kstart_known ? "" : " (unreadable)",
                (unsigned long long)prior_started, (unsigned long long)boottime,
                verdict == PS5UPLOAD2_REAP_YES ? "ours, this boot"
                : verdict == PS5UPLOAD2_REAP_NOT_OURS ? "not one of ours (recycled pid), skipping"
                : "cannot show it is of this boot: NOT killing");
        if (verdict != PS5UPLOAD2_REAP_YES) return;
    }

    /* Graceful first. The takeover request (frame or flag file) already asked it to exit, and a
     * helper stopping cleanly finishes its Sony call and closes its journals; a SIGKILL while it is
     * inside a Sony call is the thing that hangs a console. So wait, bounded, for it to go by itself
     * and kill only what is still there after that. */
    {
        struct timespec t0, now;
        int gone = 0;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        for (;;) {
            if (kill((pid_t)prior, 0) != 0) {
                gone = 1;
                break;
            }
            clock_gettime(CLOCK_MONOTONIC, &now);
            if ((now.tv_sec - t0.tv_sec) * 1000 + (now.tv_nsec - t0.tv_nsec) / 1000000 >= REAP_GRACE_MS) break;
            usleep(100000);
        }
        if (gone) {
            fprintf(stderr, "[payload2] prior instance pid=%d exited by itself\n", prior);
            return;
        }
        fprintf(stderr, "[payload2] prior instance pid=%d is still alive after %d ms of grace\n", prior, REAP_GRACE_MS);
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

/* NOTE on the missing boot-session guard: runtime_reap_prior_instance
 * needs one because it reads a pid out of the ownership file, which lives
 * on persistent /data and SURVIVES reboots — after a reboot the kernel's
 * pid counter resets, so an old record's pid may now belong to unrelated
 * homebrew, and the started_at/boottime comparison is what rules that out.
 *
 * This function has no such gap to guard against. It never reads a
 * persisted pid at all — every pid it considers comes from a live
 * KERN_PROC_PROC sysctl snapshot taken right here, right now. A pid that
 * exists in that snapshot is, by construction, a pid running in the CURRENT
 * boot session; there is no stale-record case to defend against. Adding a
 * started_at/boottime check here would have nothing meaningful to compare
 * against (a live process has no persisted start time to read) and would
 * be pure dead weight. The live snapshot IS the boot-session proof. */
int runtime_sweep_our_instances(void) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    int me = (int)getpid();
    int killed = 0;
    /* pids we sent SIGKILL to, so the confirmation poll below can check each
     * one individually instead of just sleeping and hoping. A fixed cap is
     * fine — this is "our own stray instances", never an unbounded set. */
    pid_t killed_pids[64];
    int killed_pid_count = 0;

    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0 || buf_size == 0) return 0;
    size_t alloc = buf_size + (buf_size / 4) + 1024;
    uint8_t *kbuf = (uint8_t *)malloc(alloc);
    if (!kbuf) return 0;
    size_t got = alloc;
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        free(kbuf);
        return 0;
    }

    const size_t MIN_KINFO_BYTES = KINFO_TDNAME_OFFSET + 1;
    for (uint8_t *p = kbuf; (size_t)(p - kbuf) + sizeof(int) <= got;) {
        int ki_structsize = *(int *)p;
        if (ki_structsize <= 0 ||
            (size_t)ki_structsize < MIN_KINFO_BYTES ||
            (size_t)(p - kbuf) + (size_t)ki_structsize > got) {
            break;
        }
        pid_t pid = *(pid_t *)&p[KINFO_PID_OFFSET];
        const char *tdname = (const char *)&p[KINFO_TDNAME_OFFSET];
        size_t name_max = (size_t)ki_structsize - KINFO_TDNAME_OFFSET;
        char name[64] = {0};
        size_t i = 0;
        for (; i < name_max && i + 1 < sizeof(name) && tdname[i]; ++i) {
            name[i] = tdname[i];
        }
        name[i] = '\0';
        p += (size_t)ki_structsize;

        if ((int)pid <= 1 || (int)pid == me) continue;
        if (!proc_name_is_ours(name)) continue;

        fprintf(stderr,
                "[payload2] sweep: SIGKILL pid=%d name=%s (ports still held after "
                "handshake and reap both failed)\n",
                (int)pid, name);
        if (kill(pid, SIGKILL) == 0) {
            killed++;
            if (killed_pid_count < (int)(sizeof(killed_pids) / sizeof(killed_pids[0]))) {
                killed_pids[killed_pid_count++] = pid;
            }
        }
    }
    free(kbuf);

    /* Same confirmation poll runtime_reap_prior_instance uses: kill()
     * returning 0 only means the signal was DELIVERED, not that the process
     * died — a kernel-wedged process (uninterruptible sleep) ignores SIGKILL
     * entirely. Poll each killed pid for up to ~1 s and drop it out of the
     * "still alive" set as soon as it's gone, instead of a flat sleep that
     * neither confirms nor names a survivor. This is the whole point of the
     * branch: the next bug bundle should say whether the kill actually
     * worked, not just that we asked for it. */
    for (int i = 0; i < 20 && killed_pid_count > 0; i++) {
        int remaining = 0;
        for (int j = 0; j < killed_pid_count; j++) {
            if (killed_pids[j] == 0) continue; /* already confirmed dead */
            if (kill(killed_pids[j], 0) != 0) {
                killed_pids[j] = 0; /* confirmed dead */
                continue;
            }
            remaining++;
        }
        if (remaining == 0) break;
        usleep(50000);
    }
    for (int j = 0; j < killed_pid_count; j++) {
        if (killed_pids[j] == 0) continue;
        fprintf(stderr,
                "[payload2] sweep: pid=%d survived SIGKILL (kernel-wedged) — "
                "a PS5 reboot is required to clear it\n",
                (int)killed_pids[j]);
    }
    fprintf(stderr, "[payload2] sweep: killed %d instance(s)\n", killed);
    return killed;
}

/* ── Shutdown watchdog ────────────────────────────────────────────────────── */

/* The exit watchdog: it may exit from WATCHDOG_BASE_MS once no Sony call is in flight, and at all
 * events from WATCHDOG_CEILING_MS. */
#define WATCHDOG_BASE_MS 8000
#define WATCHDOG_CEILING_MS 60000
static int g_watchdog_exit_code = 0;
/* Set once by runtime_arm_shutdown_watchdog, before the watchdog thread is
 * created — never mutated after, so the watchdog thread reads it race-free
 * without a lock. May be NULL. */
static const runtime_state_t *g_watchdog_state = NULL;

static void *runtime_shutdown_watchdog(void *arg) {
    /* Name this thread: the process listing shows a representative
     * thread that is not reliably main, so an unnamed worker makes the
     * whole process read as "payload.elf". See proc_identity.h. */
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    (void)arg;
    /* Grace period for the normal close+join below. If shutdown is healthy the
     * process exits via main()'s return long before this fires and this thread
     * dies with it. If shutdown WEDGES (e.g. pthread_join on a management worker
     * stuck in an uninterruptible Sony API never returns), force the process
     * out so it can't linger as an orphan the next resend would duplicate.
     *
     * But never while a Sony call is in flight: a call cut by _exit can wedge the console, and
     * ava1_payload_stop promises not to return (or exit) inside one. So after the base time the
     * watchdog waits for sony_api_lock to be free (and HOLDS it from then on, so no new call starts
     * while the process goes down), up to a hard ceiling that it logs loudly (final review: console). */
    {
        struct timespec t0, now;
        int announced = 0, held = 0, d;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        for (;;) {
            long long elapsed;
            int busy;
            usleep(100000);
            clock_gettime(CLOCK_MONOTONIC, &now);
            elapsed = (long long)(now.tv_sec - t0.tv_sec) * 1000 + (now.tv_nsec - t0.tv_nsec) / 1000000;
            if (elapsed < WATCHDOG_BASE_MS) continue;
            busy = pthread_mutex_trylock(&sony_api_lock) != 0;
            held = !busy;
            d = ava1_exit_decide(elapsed, busy, WATCHDOG_BASE_MS, WATCHDOG_CEILING_MS);
            if (d == AVA1_EXIT_OK) break;
            if (d == AVA1_EXIT_FORCED) {
                fprintf(stderr,
                        "[payload2] SHUTDOWN WATCHDOG: a Sony call is STILL in flight after %lld ms; exiting anyway "
                        "(hard ceiling %d ms). The console may need a restart.\n",
                        elapsed, WATCHDOG_CEILING_MS);
                break;
            }
            if (held) pthread_mutex_unlock(&sony_api_lock); /* not at the exit yet: do not block the others */
            if (!announced) {
                announced = 1;
                fprintf(stderr,
                        "[payload2] shutdown watchdog: %d ms passed but a Sony call is in flight; waiting for it "
                        "(up to %d ms)\n",
                        WATCHDOG_BASE_MS, WATCHDOG_CEILING_MS);
            }
        }
        (void)held; /* when held, the lock stays taken: _exit below, nothing else runs a Sony call */
    }
    /* Journals are fsynced on every append; make the last ones certain, bounded (an fsync on a wedged
     * drive must not stop the forced exit this thread exists to guarantee). */
    if (ava1_exit_flush(2000) != 0) fprintf(stderr, "[payload2] shutdown watchdog: journal flush abandoned after 2 s\n");
    /* We are exiting deliberately — clear the ownership record BEFORE
     * _exit() so the next instance doesn't read a leftover record + dead
     * pid as `killed_externally`. runtime_clear_ownership is just unlink()
     * + fprintf on failure: no locks, no allocation, can't block, so this
     * can't itself wedge the forced exit it exists to guarantee. */
    if (g_watchdog_state) {
        (void)runtime_clear_ownership(g_watchdog_state);
    }
    fprintf(stderr, "[payload2] shutdown watchdog fired — forcing _exit(%d)\n",
            g_watchdog_exit_code);
    _exit(g_watchdog_exit_code);
    return NULL;
}

void runtime_arm_shutdown_watchdog(const runtime_state_t *state, int exit_code) {
    g_watchdog_state = state;
    g_watchdog_exit_code = exit_code;
    pthread_t t;
    if (pthread_create(&t, NULL, runtime_shutdown_watchdog, NULL) == 0) {
        pthread_detach(t);
    }
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
 * refused, so `/data/ps5upload/runtime`, `/data/ps5upload/ava`, and any
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
    if (path_in_protected(path) || path_contains_protected(path)) return 0; /* the AVA1 trust store */

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

static int handle_cleanup(runtime_state_t *state, const char *request_body, uint64_t body_len) {
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
        return mgmt_reply(MGMT_FRAME_ERROR, "cleanup_missing_path", 20);
    }
    if (!cleanup_path_allowed(path)) {
        fprintf(stderr, "[payload2] cleanup: refusing disallowed path %s\n", path);
        return mgmt_reply(MGMT_FRAME_ERROR, "cleanup_path_denied", 19);
    }
    rc = remove_recursive_path(path, &removed_files, &removed_dirs);
    if (rc != 0) {
        fprintf(stderr, "[payload2] cleanup: remove_recursive_path(%s) failed errno=%d\n",
                path, errno);
        return mgmt_reply(MGMT_FRAME_ERROR, "cleanup_io_error", 16);
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
    return mgmt_reply(MGMT_FRAME_CLEANUP_ACK, resp, (uint64_t)len);
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

/* Mirrors ps5upload-core::volumes::{safety_reserve_bytes,
 * external_reserve_for_total}. The host falls back to the SAME rule when an
 * older payload omits the published field, so the two must not drift.
 *
 * The margin is a CAP scaled to the volume, not a flat charge: a flat 1 GiB
 * on a 64 MiB mounted disk image reserves sixteen times its own capacity,
 * drives allocatable to zero, and refuses every write to it
 * (hardware-confirmed — a 29-byte write into a mounted 64 MiB image was
 * rejected as "have 0 bytes"). Internal storage is no longer a special case;
 * the body says why. */
static uint64_t capacity_reserve_for_mount(const char *mnt_on,
                                           const char *mnt_from,
                                           uint64_t total_bytes) {
    uint64_t scaled;
    /* Internal storage (/data, /user) used to get a flat 80 GiB here, meant
     * to model the PS5 content allocator's hidden pool. It was fitted to one
     * FW 12.00 capture and did not generalise: this gate then rejected a
     * 2.5 GiB pkg on a console with 86 GB free, and a 70 GB game on one with
     * 136 GB free. The pool is real, but its size is not predictable from
     * anything readable here -- and this gate REFUSES the transfer, so it now
     * blocks only what is certainly impossible. A console that overstates its
     * free space is caught mid-write instead, where the host turns that into
     * a plain-language error (ps5upload-core::transfer::capacity_exhausted_body). */
    (void)mnt_on;
    (void)mnt_from;
    /* A volume reporting no total reserves nothing — better to let the write
     * be attempted than to block it on missing telemetry. */
    scaled = total_bytes / 64u;
    return scaled < PS5UPLOAD2_EXTERNAL_SPACE_RESERVE
               ? scaled
               : PS5UPLOAD2_EXTERNAL_SPACE_RESERVE;
}

static int handle_fs_list_volumes(runtime_state_t *state) {
    const size_t RESP_CAP = 64u * 1024u; /* heap; ~330 B per mount, was 16 KiB */
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
     * refresh, which is much better than a wedged management worker. The
     * FS_UNMOUNT handler already uses MNT_NOWAIT for the same reason. */
    nmnts = mntinfo_snapshot(&mnts);
    if (nmnts < 0 || mnts == NULL) {
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_list_volumes_getmntinfo_failed", 33);
    }

    resp = (char *)malloc(RESP_CAP);
    if (!resp) {
        free(mnts);
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_list_volumes_oom", 19);
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
        uint64_t safety_reserve =
            capacity_reserve_for_mount(mnt_on, mnt_from, total);
        uint64_t allocatable =
            avail > safety_reserve ? avail - safety_reserve : 0;
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
                     "\"safety_reserve_bytes\":%llu,"
                     "\"allocatable_bytes\":%llu,"
                     "\"writable\":%s,\"is_placeholder\":false,"
                     "\"source_image\":\"%s\"}",
                     first_volume ? "" : ",",
                     path_esc,
                     from_esc,
                     mnts[i].f_fstypename,
                     (unsigned long long)total,
                     (unsigned long long)avail,
                     (unsigned long long)safety_reserve,
                     (unsigned long long)allocatable,
                     writable ? "true" : "false",
                     source_esc);
        if (n < 0 || (size_t)n >= RESP_CAP - off) {
            /* Never answer with a clipped list (SPEC.md §7.3): the old `break`
             * sent the volumes that fit as a success and silently dropped the
             * rest. */
            fprintf(stderr, "[payload2] fs_list_volumes: response buffer full at %d/%d mounts\n",
                    i, nmnts);
            free(resp);
            free(mnts);
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_list_volumes_reply_truncated", 31);
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
    rc = mgmt_reply(MGMT_FRAME_FS_LIST_VOLUMES_ACK, resp, (uint64_t)off);
    free(resp);
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
    /* The lexical rule on the path and on its canonical form (symlink-escape guard, CWE-59), with
     * the deepest existing ancestor resolved for a path that does not exist yet: see path_policy.h. */
    return path_resolve_allowed(p, is_path_lexically_allowed);
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
    if (path_has_dotdot_component(p)) return 0;
    size_t n = strlen(p);
    return (n >= 11 && strcmp(p + n - 11, "/avatar.png") == 0) ||
           (n >= 12 && strcmp(p + n - 12, "/picture.png") == 0);
}

/* Validate a path for unsafe read mode. This relaxes the writable-root
 * allowlist so system partitions become readable, but three guards stay:
 *   (1) Path must be absolute, no dotdot, no relative segments.
 *   (2) The lexical path must be under one of the known system read-only
 *       partitions — not /proc, /dev, /kmem, or arbitrary kernel VFS.
 *   (3) realpath() resolves symlinks; the canonical form must still
 *       be under an allowed partition. A symlink inside /data that
 *       points to /dev/kmem is rejected here. (Same CWE-59 guard
 *       is_path_allowed uses for the writable roots.)
 *
 * If the target file doesn't exist yet (realpath returns NULL), we
 * fall back to the lexical check — reading a non-existent file just
 * yields fs_read_stat_failed downstream, there's no symlink to follow. */
static int is_unsafe_read_root_allowed(const char *p) {
    if (strncmp(p, "/system/",      8) == 0) return 1;
    if (strncmp(p, "/system_data/", 13) == 0) return 1;
    if (strncmp(p, "/system_ex/",   11) == 0) return 1;
    /* Bare roots (no trailing slash) for the list-dir / stat case. */
    if (strcmp(p, "/system") == 0) return 1;
    if (strcmp(p, "/system_data") == 0) return 1;
    if (strcmp(p, "/system_ex") == 0) return 1;
    return 0;
}
int is_safe_unsafe_read_path(const char *p) {
    if (!p || p[0] != '/') return 0;
    if (path_has_dotdot_component(p)) return 0;
    if (!is_unsafe_read_root_allowed(p)) return 0;
    /* Symlink-escape guard: resolve to canonical form and re-check.
     * Mirrors the realpath guard in is_path_allowed. If the file
     * doesn't exist yet, realpath fails — no symlink to follow, so
     * accept the lexical decision (the subsequent stat() will fail). */
    char resolved[PATH_MAX];
    if (realpath(p, resolved) == NULL) return 1;
    if (!is_unsafe_read_root_allowed(resolved)) {
        fprintf(stderr,
                "[payload2] is_safe_unsafe_read_path REJECTED: %s resolves "
                "to %s (symlink escape outside system partitions)\n",
                p, resolved);
        return 0;
    }
    return 1;
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
static int handle_fs_mount(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_path_not_allowed", 25);
    }
    if (stat(image_path, &st) != 0 || !S_ISREG(st.st_mode)) {
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_image_not_a_file", 25);
    }
    if (fs_mount_detect_fstype(image_path, fstype, sizeof(fstype)) != 0) {
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_unsupported_format", 27);
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
            return mgmt_reply(MGMT_FRAME_ERROR, errbuf, (uint64_t)el);
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
            return mgmt_reply(MGMT_FRAME_FS_MOUNT_ACK, resp, (uint64_t)n);
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
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_path_not_allowed", 25);
        }
        size_t mp_len = strlen(mount_point);
        if (mp_len > 1 && mount_point[mp_len - 1] == '/') {
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_bad_mount_point", 24);
        }
        if (strcmp(mount_point, FS_MOUNT_BASE) == 0) {
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_bad_mount_point", 24);
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
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_bad_name", 17);
        }
        n = snprintf(mount_point, sizeof(mount_point), "%s/%s",
                     FS_MOUNT_BASE, mount_name);
        if (n < 0 || (size_t)n >= sizeof(mount_point)) {
            return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_name_too_long", 22);
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
            return mgmt_reply(MGMT_FRAME_ERROR, errbuf, (uint64_t)el);
        }
        unit_id = md_unit;
        snprintf(devname, sizeof(devname), "/dev/md%d", unit_id);
    }
    if (fs_mount_wait_node(devname, 1) != 0) {
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_dev_node_missing", 25);
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
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_mount_mkdir_failed", 21);
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
        return mgmt_reply(MGMT_FRAME_ERROR, errbuf, (uint64_t)len);
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
            return mgmt_reply(MGMT_FRAME_ERROR, post_err, (uint64_t)el);
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
    return mgmt_reply(MGMT_FRAME_FS_MOUNT_ACK, resp, (uint64_t)n);
}

/* ── FS_UNMOUNT handler ─────────────────────────────────────────────────
 *
 * Only lets callers unmount things under /mnt/ps5upload/ so a malicious
 * or mistaken request can't tear down system mounts or mounts owned
 * by other tools. Resolves which backend (LVD or MD) backs the mount
 * via getmntinfo
 * and dispatches to the matching detach helper — skipping that would
 * leak the attachment per mount/unmount cycle. */
static int handle_fs_unmount(runtime_state_t *state, const char *request_body,
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
    /* 3. A game sandbox's unionfs library overlay. A backport tool
     *    (BackPork) mounts `fakelib` over `common/lib` at launch and only
     *    unmounts it when the game exits cleanly — which a game that
     *    crashes at load never does. The leftover then blocks the shell
     *    from removing the sandbox AND makes the kernel refuse every
     *    later mount of the same source, so the title cannot be launched
     *    again until the console is restarted. There is no tracker file
     *    for it because we did not create it; the path shape and the
     *    filesystem type are the consent record instead. See
     *    include/sandbox_unmount.h for why that is narrow enough. */
    int sandbox_overlay = 0;
    if (!legacy_match && !mount_tracker_exists(mount_point) &&
        !path_has_dotdot_component(mount_point)) {
        struct statfs *probe = NULL;
        int nprobe = mntinfo_snapshot(&probe);
        for (int i = 0; i < nprobe && probe != NULL; i++) {
            if (strcmp(probe[i].f_mntonname, mount_point) != 0) continue;
            sandbox_overlay =
                sandbox_union_unmount_allowed(mount_point, probe[i].f_fstypename);
            break;
        }
        free(probe);
    }

    if (path_has_dotdot_component(mount_point) ||
        (!legacy_match && !sandbox_overlay && !mount_tracker_exists(mount_point))) {
        return mgmt_reply(MGMT_FRAME_ERROR, "fs_unmount_not_our_mount", 24);
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
        return mgmt_reply(MGMT_FRAME_ERROR, reason, reason_len);
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

    return mgmt_reply(MGMT_FRAME_FS_UNMOUNT_ACK, NULL, 0);
}

/* ── APP_* handlers (thin wrappers over register.c) ─────────────────────
 *
 * The heavy lifting lives in payload/src/register.c. These handlers
 * only parse the request body, call into the register module, and
 * turn the returned err_reason
 * string into an ERROR frame. Each handler bumps command_count so
 * STATUS_ACK reflects management-port activity. */

static int handle_app_register(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "register_src_path_missing", 25);
    }
    if (!is_path_allowed(src_path)) {
        return mgmt_reply(MGMT_FRAME_ERROR, "register_src_path_not_allowed", 29);
    }
    if (register_title_from_path(src_path, patch_drm_type, title_id,
                                  title_name, &used_nullfs, &err) != 0) {
        const char *reason = err ? err : "register_failed";
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
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
        return mgmt_reply(MGMT_FRAME_ERROR, "register_response_overflow", 26);
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

    return mgmt_reply(MGMT_FRAME_APP_REGISTER_ACK, resp, (uint64_t)n);
}

static int handle_app_unregister(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "unregister_title_id_missing", 27);
    }
    unsigned sony_rc = 0u;
    if (unregister_title(title_id, &err, &sony_rc) != 0) {
        const char *reason = err ? err : "unregister_failed";
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Unregistered %s", title_id);
        pop_notification(toast);
    }

    /* Carry Sony's uninstall result back. An empty ACK used to mean
     * "unregistered" unconditionally, so a refusal by
     * sceAppInstUtilAppUninstall (e.g. 0x80B21B02 on a title whose
     * content records are inconsistent) was invisible to the client. */
    {
        char ack[96];
        int alen = snprintf(ack, sizeof(ack),
                            "{\"sony_uninstall_rc\":%u}", sony_rc);
        if (alen < 0) alen = 0;
        return mgmt_reply(MGMT_FRAME_APP_UNREGISTER_ACK, ack, (uint64_t)alen);
    }
}

static int handle_app_launch(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "launch_title_id_missing", 23);
    }
    /* Stack scratch for launch_title's formatted failure reasons —
     * per-call storage so concurrent APP_LAUNCH management workers can't
     * race each other's error strings (see register.h). */
    char launch_reason[128];
    if (launch_title(title_id, &err, launch_reason, sizeof(launch_reason)) !=
        0) {
        const char *reason = err ? err : "launch_failed";
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    {
        char toast[160];
        snprintf(toast, sizeof(toast), "Launching %s", title_id);
        pop_notification(toast);
    }

    return mgmt_reply(MGMT_FRAME_APP_LAUNCH_ACK, NULL, 0);
}

static int handle_app_list_registered(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "list_registered_oom", 19);
    }
    if (list_registered_titles_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "list_registered_failed";
        rc = mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = mgmt_reply(MGMT_FRAME_APP_LIST_REGISTERED_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Hardware monitoring handlers (thin wrappers over hw_info.c) ──────── */

static int handle_hw_text_op(runtime_state_t *state, int (*getter)(char *, size_t, size_t *, const char **),
                              uint16_t ack_type, const char *default_err) {
    char body[2048];
    size_t written = 0;
    const char *err = NULL;
    if (!state) return -1;
    if (getter(body, sizeof(body), &written, &err) != 0) {
        const char *reason = err ? err : default_err;
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(ack_type, body, (uint64_t)written);
}

static int handle_hw_info(runtime_state_t *state) {
    return handle_hw_text_op(state, hw_info_get_text,
                              MGMT_FRAME_HW_INFO_ACK, "hw_info_failed");
}

/* HW_TEMPS. A request body of "1" selects the EXTENDED read (SoC power /
 * CPU usage / fan duty / product shape) used by the explicit "Read
 * sensors" click; an empty body (the Dashboard's 5 s auto-poll) gets the
 * BASIC, always-safe read. Gating the risky getters on the body keeps
 * them off every auto-poll path — see hw_temps_get_text_ex. */
static int handle_hw_temps(runtime_state_t *state, const char *request_body, uint64_t body_len) {
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
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_HW_TEMPS_ACK, body, (uint64_t)written);
}

static int handle_hw_power(runtime_state_t *state) {
    return handle_hw_text_op(state, hw_power_get_text,
                              MGMT_FRAME_HW_POWER_ACK, "hw_power_failed");
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

static int handle_time_get(runtime_state_t *state) {
    if (!state) return -1;
    sce_datetime_t dt;
    memset(&dt, 0, sizeof(dt));
    uint32_t ec = 0;
    pthread_mutex_lock(&sony_api_lock); /* sceSystemServiceGetCurrentDateTime: one Sony call at a time */
    int rc = sys_time_get(&dt, &ec);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
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
        return mgmt_reply(MGMT_FRAME_TIME_GET_ACK, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_TIME_GET_ACK, body, (uint64_t)n);
}

static int handle_time_set(runtime_state_t *state, const char *request_body, uint64_t body_len) {
    if (!state) return -1;
    if (!request_body || body_len == 0) {
        const char *err = "{\"ok\":false,\"err_code\":3758104577}"; /* SYS_TIME_ERR_NULL_ARG */
        return mgmt_reply(MGMT_FRAME_TIME_SET_ACK, err, (uint64_t)strlen(err));
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
        return mgmt_reply(MGMT_FRAME_TIME_SET_ACK, err, (uint64_t)strlen(err));
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
    int used_fallback = 0;
    pthread_mutex_lock(&sony_api_lock); /* sceSystemServiceGet/SetCurrentDateTime: one Sony call at a time */
    int rc = sys_time_set(&dt, &ec, &prior_unix, &new_unix, &used_fallback);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
    char body[256];
    /* snake_case keys: the engine deserializes this with serde, which
     * silently zeroes any field whose name doesn't match. */
    int n = snprintf(body, sizeof(body),
                     "{\"ok\":%s,\"err_code\":%u,"
                     "\"prior_unix\":%lld,\"new_unix\":%lld,"
                     "\"used_fallback\":%s}",
                     rc == 0 ? "true" : "false",
                     (unsigned)ec,
                     (long long)prior_unix,
                     (long long)new_unix,
                     used_fallback ? "true" : "false");
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *fb = "{\"ok\":false,\"err_code\":0}";
        return mgmt_reply(MGMT_FRAME_TIME_SET_ACK, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_TIME_SET_ACK, body, (uint64_t)n);
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

static int handle_time_state_get(runtime_state_t *state) {
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

    /* sceRegMgr is not safe to call concurrently (CE-108262-9): hold sony_api_lock for the registry
     * reads (and none of them takes it), and build/send the reply after releasing it. */
    pthread_mutex_lock(&sony_api_lock);
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
    int wall_rc        = sys_time_get(&wall_dt, &wall_err); /* a Sony call: still under the lock */
    pthread_mutex_unlock(&sony_api_lock);

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
    return mgmt_reply(MGMT_FRAME_TIME_STATE_GET_ACK, body, (uint64_t)(p - body));

truncated: {
    /* Last-resort fallback — any field above blew the buffer.
     * Shouldn't happen at 2 KB but the alternative (return -1 and
     * drop the connection) is worse for the user than a stub
     * response. */
    const char *fb = "{\"ok\":false,\"err_code\":0,\"truncated\":true}";
    return mgmt_reply(MGMT_FRAME_TIME_STATE_GET_ACK, fb, (uint64_t)strlen(fb));
}
}

static int handle_time_state_set(runtime_state_t *state, const char *request_body,
                                   uint64_t body_len) {
    if (!state) return -1;
    if (!request_body || body_len == 0) {
        const char *err = "{\"ok\":false,\"err_code\":3758108673}"; /* SYS_REGISTRY_ERR_NULL_ARG */
        return mgmt_reply(MGMT_FRAME_TIME_STATE_SET_ACK, err, (uint64_t)strlen(err));
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
    pthread_mutex_lock(&sony_api_lock); /* sceRegMgr: one caller at a time (CE-108262-9) */
    if (has_tz)       rc_tz     = sys_registry_set_int(SCE_KEY_DATE_TIME_ZONE,    (int)tz_idx,     &ec_tz);
    if (has_date_fmt) rc_date   = sys_registry_set_int(SCE_KEY_DATE_DATE_FORMAT,  (int)date_fmt,   &ec_date);
    if (has_time_fmt) rc_time   = sys_registry_set_int(SCE_KEY_DATE_TIME_FORMAT,  (int)time_fmt,   &ec_time);
    if (has_summer)   rc_summer = sys_registry_set_int(SCE_KEY_DATE_SUMMER_TIME,  (int)summer,     &ec_summer);
    if (has_set_auto) rc_auto   = sys_registry_set_int(SCE_KEY_DATE_SET_AUTO,     (int)set_auto,   &ec_auto);
    pthread_mutex_unlock(&sony_api_lock);

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
        return mgmt_reply(MGMT_FRAME_TIME_STATE_SET_ACK, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_TIME_STATE_SET_ACK, body, (uint64_t)n);
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

static int handle_smp_meta_control(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_SMP_META_CONTROL_ACK, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_SMP_META_CONTROL_ACK, body, (uint64_t)n);
}

static int handle_smp_meta_stats(runtime_state_t *state) {
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
        return mgmt_reply(MGMT_FRAME_SMP_META_STATS_ACK, fb, (uint64_t)strlen(fb));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_SMP_META_STATS_ACK, body, (uint64_t)n);
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

/* Graceful "close the game" entry points from libSceSystemService — the
 * library the payload already links, so these resolve in-process with no
 * ptrace and no extra dlopen.
 *
 * Needed because libSceSysCore's application APIs are BLIND on at least
 * FW 9.60: with a game demonstrably running, sceApplicationGetProcs returns
 * an EMPTY list and sceApplicationKill rejects the kernel app id with
 * 0x80AA0004. That left the UI falling back to SIGKILL on the pid — which
 * stops the game but isn't what the console does when a user picks
 * "Close game".
 *
 * LncUtil is the launcher util that owns the other half of this pair: we
 * already start titles with sceLncUtilLaunchApp, so its kill counterpart is
 * the symmetric, intended way to stop one. */
typedef int (*sce_lnc_kill_fn)(unsigned int app_id);
static sce_lnc_kill_fn p_sceLncUtilKillApp        = NULL;
static sce_lnc_kill_fn p_sceLncUtilForceKillApp   = NULL;
static sce_lnc_kill_fn p_sceSystemServiceKillApp  = NULL;
static int lnc_kill_resolve_attempted = 0;

static void resolve_lnc_kill(void) {
    if (lnc_kill_resolve_attempted) return;
    lnc_kill_resolve_attempted = 1;
    dlerror();
    p_sceLncUtilKillApp = (sce_lnc_kill_fn)dlsym(RTLD_DEFAULT, "sceLncUtilKillApp");
    if (!p_sceLncUtilKillApp) (void)dlerror();
    p_sceLncUtilForceKillApp =
        (sce_lnc_kill_fn)dlsym(RTLD_DEFAULT, "sceLncUtilForceKillApp");
    if (!p_sceLncUtilForceKillApp) (void)dlerror();
    p_sceSystemServiceKillApp =
        (sce_lnc_kill_fn)dlsym(RTLD_DEFAULT, "sceSystemServiceKillApp");
    if (!p_sceSystemServiceKillApp) (void)dlerror();
    fprintf(stderr,
            "[payload2] kill APIs: LncUtilKillApp=%s ForceKillApp=%s "
            "SystemServiceKillApp=%s\n",
            p_sceLncUtilKillApp ? "yes" : "no",
            p_sceLncUtilForceKillApp ? "yes" : "no",
            p_sceSystemServiceKillApp ? "yes" : "no");
}
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

/* power.control over AVA1: the handler is called with its reply captured, and the reply only leaves
 * after the handler returns. A reboot/shutdown/standby that ran inside the handler would take the
 * network down before the ACK was sent, so under the capture sink the destructive call is deferred to
 * a short-lived thread that waits for the ACK to flush (the old contract: reply BEFORE the call). */
typedef struct {
    int action; /* system_control_action_t */
} power_defer_t;

/* Runs on the deferred power thread only, which holds no lock: take sony_api_lock so a reboot or
 * power-off never overlaps another Sony call (the lock is held until the console goes down). */
static void power_do_action(int action) {
    pthread_mutex_lock(&sony_api_lock);
    if (action == SC_ACTION_REBOOT) {
        sceSystemServiceRequestReboot();
    } else if (action == SC_ACTION_SHUTDOWN) {
        void *h = dlsym(RTLD_DEFAULT, "sceSystemStateMgrTurnOff");
        if (h) {
            int (*turn_off)(int) = (int (*)(int))h;
            turn_off(0);
        } else {
            sceSystemServiceRequestPowerOff();
        }
    } else if (action == SC_ACTION_STANDBY) {
        void *h = dlsym(RTLD_DEFAULT, "sceSystemStateMgrEnterStandby");
        if (h) {
            int (*enter_standby)(void) = (int (*)(void))h;
            enter_standby();
        }
    }
    pthread_mutex_unlock(&sony_api_lock);
}

static void *power_defer_thread(void *arg) {
    power_defer_t *d = (power_defer_t *)arg;
    int action = d->action;
    free(d);
    usleep(400 * 1000); /* the ACK frame is written by the session thread as soon as the handler returns */
    power_do_action(action);
    return NULL;
}

/* 0 when the action will run (deferred); -1 when it could not be scheduled (the caller runs it inline). */
static int power_defer(int action) {
    power_defer_t *d = malloc(sizeof *d);
    pthread_t t;
    if (!d) return -1;
    d->action = action;
    if (create_worker_thread(&t, power_defer_thread, d) != 0) {
        free(d);
        return -1;
    }
    pthread_detach(t);
    return 0;
}

static int handle_system_control(runtime_state_t *state, const char *body,
                                  uint64_t body_len) {
    if (!state) return -1;
    system_control_action_t action;
    if (parse_system_control_action(body, body_len, &action) != 0) {
        const char *err = "{\"ok\":false,\"err\":\"bad_action\"}";
        return mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, err, strlen(err));
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
        rc = mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, ack, strlen(ack));
        if (mgmt_capture_active() && power_defer(SC_ACTION_REBOOT) == 0) return rc;
        power_do_action(SC_ACTION_REBOOT); /* takes sony_api_lock (legacy path and power_defer failure) */
        return rc;
    }
    case SC_ACTION_SHUTDOWN: {
        const char *ack = "{\"ok\":true,\"action\":\"shutdown\"}";
        rc = mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, ack, strlen(ack));
        if (mgmt_capture_active() && power_defer(SC_ACTION_SHUTDOWN) == 0) return rc;
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
        power_do_action(SC_ACTION_SHUTDOWN); /* sceSystemStateMgrTurnOff, else RequestPowerOff, under sony_api_lock */
        return rc;
    }
    case SC_ACTION_STANDBY: {
        /* sceSystemStateMgrEnterStandby is dlsym'd to handle FW where
         * the symbol moved or doesn't exist. */
        void *h = dlsym(RTLD_DEFAULT, "sceSystemStateMgrEnterStandby");
        if (!h) {
            err_str = "{\"ok\":false,\"err\":\"standby_unavailable\"}";
            return mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, err_str, strlen(err_str));
        }
        const char *ack = "{\"ok\":true,\"action\":\"standby\"}";
        rc = mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, ack, strlen(ack));
        if (mgmt_capture_active() && power_defer(SC_ACTION_STANDBY) == 0) return rc;
        power_do_action(SC_ACTION_STANDBY);
        return rc;
    }
    case SC_ACTION_TICK:
        /* Tick is non-destructive; we can ACK after. */
        pthread_mutex_lock(&sony_api_lock); /* a Sony call: one at a time with the others */
        err_code = sceSystemServicePowerTick();
        pthread_mutex_unlock(&sony_api_lock);
        if (err_code == 0) {
            const char *ack = "{\"ok\":true,\"action\":\"tick\"}";
            return mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, ack, strlen(ack));
        }
        {
            char buf[128];
            int n = snprintf(buf, sizeof(buf),
                             "{\"ok\":false,\"err\":\"power_tick_failed\","
                             "\"code\":%d}", err_code);
            return mgmt_reply(MGMT_FRAME_SYSTEM_CONTROL_ACK, buf, (size_t)n);
        }
    }
    /* Unreachable, but quiet the compiler. */
    return -1;
}

/* ── Power telemetry handler ─────────────────────────────────────────── */

static int handle_power_telemetry(runtime_state_t *state) {
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
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "operating_seconds=%u\n", op_secs);
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    } else {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "operating_seconds=err\n");
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    }
    if (rc_boot == 0) {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "boot_cycles=%u\n", boot_cycles);
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    } else {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "boot_cycles=err\n");
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    }
    if (rc_therm == 0) {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "thermal_alert_flags=%u\n", (unsigned)thermal_flags);
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    } else {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "thermal_alert_flags=err\n");
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    }
    if (rc_pwc == 0) {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "power_up_cause=%u\n", (unsigned)power_up_cause);
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    } else {
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "power_up_cause=err\n");
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    }
    /* Say WHY the values are missing, instead of returning four bare
     * nulls the UI can only render as "—".
     *
     * On retail FW 9.60 none of the four sceKernelIccGet* symbols
     * resolve via dlsym — verified on hardware. That's distinct from a
     * symbol that resolves but whose call fails, and the client should
     * be able to tell the difference: "your firmware doesn't expose
     * this" is a different message from "we asked and it errored".
     *
     * NOTE for a future attempt: /dev/icc_power and /dev/icc_device_power
     * DO exist as device nodes (confirmed by listing /dev on hardware),
     * so an ioctl path like the one fan control uses on /dev/icc_fan is
     * plausible. We don't take it because the required ioctl numbers
     * aren't documented anywhere we can verify, and guessing ioctls
     * against a live ICC device risks hanging the console for what is a
     * cosmetic readout. */
    {
        int resolved = (p_sceKernelIccGetPowerOperatingTime != NULL)
                     + (p_sceKernelIccGetPowerNumberOfBootShutdown != NULL)
                     + (p_sceKernelIccGetThermalAlert != NULL)
                     + (p_sceKernelIccGetPowerUpCause != NULL);
        /* Count successful CALLS, not resolved symbols. FW 5.10 resolves
         * all four yet two of them still return non-zero — reporting
         * "ok" off the symbol count alone claimed everything worked
         * while half the fields came back null. Resolved means "the
         * function exists"; only rc == 0 means "we got a value". */
        int ok = (rc_op == 0) + (rc_boot == 0) + (rc_therm == 0) + (rc_pwc == 0);
        const char *detail;
        if (resolved == 0)      detail = "unsupported_firmware";
        else if (ok == 0)       detail = "calls_failed";
        else if (ok < 4)        detail = "partial";
        else                    detail = "ok";
        int w = snprintf(body + n, sizeof(body) > (size_t)n ? sizeof(body) - n : 0,
                         "symbols_resolved=%d\nvalues_ok=%d\nstatus=%s\n",
                         resolved, ok, detail);
        if (w > 0) n += (size_t)w > sizeof(body) - n ? (int)(sizeof(body) - n) : w;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_POWER_TELEMETRY_ACK, body, (uint64_t)n);
}

/* ── User account enumeration ────────────────────────────────────────── */

static int handle_user_list(runtime_state_t *state) {
    if (!state) return -1;
    /* Init is idempotent — Sony's API is documented to no-op on a
     * second call. We don't track a "first call done" flag because
     * the cost is negligible and statelessness avoids cross-thread
     * locking concerns. */
    /* sceUserService is not safe to call concurrently (CE-108262-9): serialise on sony_api_lock like
     * every other caller. AVA1 runs up to 8 management calls at once, so this handler holds the lock
     * itself for its Sony calls and sends after it is released. */
    pthread_mutex_lock(&sony_api_lock);
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
    pthread_mutex_unlock(&sony_api_lock);
    if (n < (int)sizeof(body) - 2) {
        body[n++] = ']';
        body[n++] = '}';
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_USER_LIST_ACK, body, (uint64_t)n);
}

/* ── User create / delete ──────────────────────────────────────────────
 * sceUserServiceCreateUser allocates a new local user account and
 * returns its user_id. The initial name is set via SetUserName after
 * creation. sceUserServiceDeleteUser removes the account; Sony cleans
 * up home dir content but we offer an optional pre-wipe of savedata. */

static int handle_user_create(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char name[64] = {0};
    extract_json_string_field(body, "name", name, sizeof(name));
    int rc = -1;
    int new_uid = -1;
    const char *err_msg = "";
    if (name[0]) {
        pthread_mutex_lock(&sony_api_lock);
        sceUserServiceInitialize(NULL); /* a Sony call: inside the lock (final review: console) */
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
    return mgmt_reply(MGMT_FRAME_USER_CREATE_ACK, resp, (uint64_t)len);
}

static int handle_user_delete(runtime_state_t *state, const char *body) {
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
        pthread_mutex_lock(&sony_api_lock);
        sceUserServiceInitialize(NULL); /* a Sony call: inside the lock (final review: console) */
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
    return mgmt_reply(MGMT_FRAME_USER_DELETE_ACK, resp, (uint64_t)len);
}

/* ── Backup & restore ────────────────────────────────────────────────── */

static int handle_backup_snapshot(runtime_state_t *state, const char *body) {
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
        if (rc == BACKUP_CANCELLED)
            return mgmt_reply(MGMT_FRAME_ERROR, "backup_cancelled", 16);
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
    return mgmt_reply(MGMT_FRAME_BACKUP_SNAPSHOT_ACK, resp, (uint64_t)len);
}

static int handle_backup_list(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    char *buf = malloc(32768);
    if (!buf) {
        const char *e = "{\"ok\":false,\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_BACKUP_LIST_ACK, e, strlen(e));
    }
    size_t written = 0;
    backup_list(tag[0] ? tag : NULL, buf, 32768, &written);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = mgmt_reply(MGMT_FRAME_BACKUP_LIST_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_backup_restore(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char tag[64] = {0};
    extract_json_string_field(body, "tag", tag, sizeof(tag));
    int64_t ts = (int64_t)extract_json_uint64_field(body, "timestamp");
    int restored = 0;
    const char *err = "";
    int rc = -1;
    if (tag[0] && ts > 0) {
        rc = backup_restore(tag, ts, &restored);
        if (rc == BACKUP_CANCELLED)
            return mgmt_reply(MGMT_FRAME_ERROR, "backup_cancelled", 16);
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
    return mgmt_reply(MGMT_FRAME_BACKUP_RESTORE_ACK, resp, (uint64_t)len);
}

static int handle_backup_delete(runtime_state_t *state, const char *body) {
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
    return mgmt_reply(MGMT_FRAME_BACKUP_DELETE_ACK, resp, (uint64_t)len);
}

/* ── Remote Play handlers ────────────────────────────────────────────── */
static int handle_remoteplay_request(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char acct[64] = {0};
    extract_json_string_field(body, "manual_account_id", acct, sizeof(acct));
    int rc = remoteplay_request(acct[0] ? acct : NULL);
    /* Carry the PIN back in the ack.
     *
     * A caller that is about to run the registration handshake itself must
     * not learn the PIN by polling status: that call probes
     * sceRemoteplayConfirmDeviceRegist, which finalises the pending
     * registration on the console and makes the handshake that follows be
     * refused. Returning it here removes the need to poll at all. */
    char snap[192] = "";
    if (rc == 0) (void)remoteplay_pin_snapshot(snap, sizeof(snap));
    char resp[256];
    int len;
    if (rc == 0 && snap[0]) {
        len = snprintf(resp, sizeof(resp), "{\"ok\":true,\"snapshot\":%s}", snap);
    } else {
        len = snprintf(resp, sizeof(resp), "{\"ok\":%s}",
                       rc == 0 ? "true" : "false");
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_STATUS, resp, (uint64_t)len);
}

static int handle_remoteplay_status(runtime_state_t *state) {
    if (!state) return -1;
    char buf[512];
    int rc = remoteplay_get_status(buf, sizeof(buf));
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    if (rc != 0) {
        const char *e = "{\"state\":\"failed\"}";
        return mgmt_reply(MGMT_FRAME_REMOTEPLAY_STATUS, e, strlen(e));
    }
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_STATUS, buf, strlen(buf));
}

static int handle_remoteplay_cancel(runtime_state_t *state) {
    if (!state) return -1;
    remoteplay_cancel();
    const char *resp = "{\"ok\":true}";
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_CANCEL_ACK, resp, strlen(resp));
}

/* Remote Play readiness / enable / devices. They were inline in the old dispatcher; they are
 * handlers now so the AVA1 management table (rp.readiness, rp.enable, rp.devices) can call them.
 * The bodies are unchanged. The remoteplay_* entry points take sony_api_lock for the whole call
 * (remoteplay.c "Sony-API serialization"), and the reply is sent after they return. */
static int handle_remoteplay_readiness(runtime_state_t *state) {
    (void)state;
    char body[640];
    int n = remoteplay_readiness_json(body, sizeof(body));
    if (n < 0 || (size_t)n >= sizeof(body)) {
        return mgmt_reply(MGMT_FRAME_ERROR, "readiness_overflow", 18);
    }
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_READINESS, body, (uint64_t)n);
}

static int handle_remoteplay_enable(runtime_state_t *state, const char *request_body) {
    (void)state;
    char scope[16] = "";
    extract_json_string_field(request_body, "scope", scope, sizeof(scope));
    char body[640];
    int n = remoteplay_enable(strcmp(scope, "user") == 0, body, sizeof(body));
    if (n == -2) {
        return mgmt_reply(MGMT_FRAME_ERROR, "rp_enable_unsupported_fw", 24);
    }
    if (n == -3) {
        return mgmt_reply(MGMT_FRAME_ERROR, "rp_enable_no_user", 17);
    }
    if (n < 0 || (size_t)n >= sizeof(body)) {
        return mgmt_reply(MGMT_FRAME_ERROR, "rp_enable_write_failed", 22);
    }
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_ENABLE, body, (uint64_t)n);
}

static int handle_remoteplay_devices(runtime_state_t *state) {
    (void)state;
    char body[2048];
    int n = remoteplay_devices_json(body, sizeof(body));
    if (n < 0 || (size_t)n >= sizeof(body)) {
        return mgmt_reply(MGMT_FRAME_ERROR, "rp_devices_overflow", 19);
    }
    return mgmt_reply(MGMT_FRAME_REMOTEPLAY_DEVICES, body, (uint64_t)n);
}

/* ── Fan curve handler ───────────────────────────────────────────────── */
static int handle_fan_curve_set(runtime_state_t *state, const char *body) {
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
    return mgmt_reply(MGMT_FRAME_HW_FAN_CURVE_SET_ACK, resp, (uint64_t)len);
}

/* ── Notifications handler ───────────────────────────────────────────── */
static int handle_notif_list(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    uint64_t since = extract_json_uint64_field(body, "since_seq");
    char *buf = malloc(32768);
    if (!buf) {
        const char *e = "{\"notifications\":[]}";
        return mgmt_reply(MGMT_FRAME_NOTIF_LIST_ACK, e, strlen(e));
    }
    size_t written = 0;
    notif_list(since, buf, 32768, &written);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = mgmt_reply(MGMT_FRAME_NOTIF_LIST_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Activity reset handler ─────────────────────────────────────────── */
static int handle_activity_reset(runtime_state_t *state) {
    if (!state) return -1;
    int removed = activity_reset();
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char body[64];
    int n = snprintf(body, sizeof(body),
                     "{\"ok\":true,\"removed\":%d}", removed);
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *e = "{\"ok\":false,\"error\":\"format\"}";
        return mgmt_reply(MGMT_FRAME_ACTIVITY_RESET_ACK, e, strlen(e));
    }
    return mgmt_reply(MGMT_FRAME_ACTIVITY_RESET_ACK, body, (uint64_t)n);
}

/* ── Notification clear handler ─────────────────────────────────────── */
static int handle_notif_clear(runtime_state_t *state) {
    if (!state) return -1;
    int removed = notif_clear();
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char body[64];
    int n = snprintf(body, sizeof(body),
                     "{\"ok\":true,\"removed\":%d}", removed);
    if (n < 0 || (size_t)n >= sizeof(body)) {
        const char *e = "{\"ok\":false,\"error\":\"format\"}";
        return mgmt_reply(MGMT_FRAME_NOTIF_CLEAR_ACK, e, strlen(e));
    }
    return mgmt_reply(MGMT_FRAME_NOTIF_CLEAR_ACK, body, (uint64_t)n);
}

/* ── Notification send handler ──────────────────────────────────────── */
static int handle_notif_send(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    if (!body) {
        const char *err = "{\"ok\":false,\"err\":\"body_required\"}";
        return mgmt_reply(MGMT_FRAME_NOTIF_SEND_ACK, err, strlen(err));
    }
    char msg[512] = {0};
    extract_json_string_field(body, "msg", msg, sizeof(msg));
    if (msg[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"msg_required\"}";
        return mgmt_reply(MGMT_FRAME_NOTIF_SEND_ACK, err, strlen(err));
    }
    int level = (int)extract_json_uint64_field(body, "level");
    int rc = notif_send_serialised(msg, level); /* sceNotificationSend needs sony_api_lock */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *resp = (rc == 0)
        ? "{\"ok\":true}"
        : "{\"ok\":false,\"err\":\"send_failed\"}";
    return mgmt_reply(MGMT_FRAME_NOTIF_SEND_ACK, resp, strlen(resp));
}

/* ── Cheat engine handlers ──────────────────────────────────────────── */

#define CHEATS_TITLE_ID_LEN 32u
#define CHEATS_JSON_BUF_SZ  (64u * 1024u)
/* The titles list is bounded by the reply ceiling (AVA1_RPC_TEXT_MAX, 262128 bytes), not by 64 KiB:
 * a full GoldHEN/etaHEN pack lists thousands of titles. Past this it ends in "truncated":true. */
#define CHEATS_LIST_BUF_SZ  (240u * 1024u)

static void cheat_inc_cmd_count(runtime_state_t *state) {
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
}

static int handle_cheats_list(runtime_state_t *state) {
    if (!state) return -1;
    char *buf = malloc(CHEATS_LIST_BUF_SZ);
    if (!buf) {
        const char *e = "{\"titles\":[]}";
        return mgmt_reply(MGMT_FRAME_CHEATS_LIST_ACK, e, strlen(e));
    }
    size_t written = 0;
    if (cheats_list_titles(buf, CHEATS_LIST_BUF_SZ, &written) != 0) {
        const char *e = "{\"titles\":[],\"truncated\":false}";
        free(buf);
        return mgmt_reply(MGMT_FRAME_CHEATS_LIST_ACK, e, strlen(e));
    }
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_CHEATS_LIST_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_cheats_get(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char title_id[CHEATS_TITLE_ID_LEN] = {0};
    if (body) extract_json_string_field(body, "title_id",
                                        title_id, sizeof(title_id));
    char *buf = malloc(CHEATS_JSON_BUF_SZ);
    if (!buf) {
        const char *e = "{\"mods\":[],\"error\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_CHEATS_GET_ACK, e, strlen(e));
    }
    size_t written = 0;
    if (title_id[0]) {
        cheats_list_mods(title_id, buf, CHEATS_JSON_BUF_SZ, &written);
    } else {
        const char *e = "{\"mods\":[],\"error\":\"title_id_required\"}";
        free(buf);
        return mgmt_reply(MGMT_FRAME_CHEATS_GET_ACK, e, strlen(e));
    }
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_CHEATS_GET_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_cheats_toggle(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    if (!body) {
        const char *e = "{\"ok\":false,\"err\":\"body_required\"}";
        return mgmt_reply(MGMT_FRAME_CHEATS_TOGGLE_ACK, e, strlen(e));
    }
    char title_id[CHEATS_TITLE_ID_LEN] = {0};
    extract_json_string_field(body, "title_id",
                              title_id, sizeof(title_id));
    int mod_index = (int)extract_json_uint64_field(body, "index");
    int turn_on = 1;
    /* Check for "on": true/false first, fall back to "turn_on" */
    int bval = 0;
    if (extract_json_bool_field(body, "on", &bval) == 0) {
        turn_on = bval;
    }

    char err[256] = {0};
    int rc = cheats_toggle(title_id, mod_index, turn_on,
                           err, sizeof(err));
    cheat_inc_cmd_count(state);

    char resp[512];
    int len;
    if (rc == 0) {
        len = snprintf(resp, sizeof(resp),
                       "{\"ok\":true,\"title_id\":\"%s\",\"index\":%d,"
                       "\"on\":%s}",
                       title_id, mod_index, turn_on ? "true" : "false");
    } else {
        char err_esc[300];
        size_t ei = 0;
        for (const char *s = err; *s && ei + 2 < sizeof(err_esc); s++) {
            if (*s == '"' || *s == '\\') { err_esc[ei++]='\\'; err_esc[ei++]=*s; }
            else if (*s == '\n') { err_esc[ei++]='\\'; err_esc[ei++]='n'; }
            else err_esc[ei++] = *s;
        }
        err_esc[ei] = '\0';
        len = snprintf(resp, sizeof(resp),
                       "{\"ok\":false,\"err\":\"%s\"}", err_esc);
    }
    return mgmt_reply(MGMT_FRAME_CHEATS_TOGGLE_ACK, resp, (uint64_t)(len > 0 ? len : 0));
}

static int handle_cheats_delete(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char title_id[CHEATS_TITLE_ID_LEN] = {0};
    if (body) extract_json_string_field(body, "title_id",
                                        title_id, sizeof(title_id));
    char err[256] = {0};
    int rc = cheats_delete(title_id, err, sizeof(err));
    cheat_inc_cmd_count(state);
    const char *resp = (rc == 0) ? "{\"ok\":true}" : "{\"ok\":false}";
    return mgmt_reply(MGMT_FRAME_CHEATS_DELETE_ACK, resp, strlen(resp));
}

static int handle_cheats_reload(runtime_state_t *state) {
    if (!state) return -1;
    char err[256] = {0};
    int rc = cheats_reload(err, sizeof(err));
    cheat_inc_cmd_count(state);
    const char *resp = (rc == 0) ? "{\"ok\":true}" : "{\"ok\":false}";
    return mgmt_reply(MGMT_FRAME_CHEATS_RELOAD_ACK, resp, strlen(resp));
}

static int handle_cheats_status(runtime_state_t *state) {
    if (!state) return -1;
    char buf[512];
    size_t written = 0;
    cheats_status_json(buf, sizeof(buf), &written);
    cheat_inc_cmd_count(state);
    return mgmt_reply(MGMT_FRAME_CHEATS_STATUS_ACK, buf, (uint64_t)written);
}

static int handle_cheats_engine_set(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    int enabled = 0;
    if (body) {
        int bval = 0;
        if (extract_json_bool_field(body, "enabled", &bval) == 0) {
            enabled = bval;
        }
    }
    cheats_engine_set_enabled(enabled);
    cheat_inc_cmd_count(state);
    const char *resp = enabled
        ? "{\"ok\":true,\"enabled\":true}"
        : "{\"ok\":true,\"enabled\":false}";
    return mgmt_reply(MGMT_FRAME_CHEATS_ENGINE_SET_ACK, resp, strlen(resp));
}

/* ── Activity tracker handlers ──────────────────────────────────────── */
#define ACTIVITY_JSON_BUF_SZ  (128u * 1024u)

static int handle_activity_get(runtime_state_t *state) {
    if (!state) return -1;
    char *buf = malloc(ACTIVITY_JSON_BUF_SZ);
    if (!buf) {
        const char *e = "{\"titles\":[]}";
        return mgmt_reply(MGMT_FRAME_ACTIVITY_GET_ACK, e, strlen(e));
    }
    size_t written = 0;
    activity_get_json(buf, ACTIVITY_JSON_BUF_SZ, &written);
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_ACTIVITY_GET_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_activity_db_query(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char query[64] = {0};
    if (body) {
        extract_json_string_field(body, "query", query, sizeof(query));
    }
    if (!query[0]) {
        strncpy(query, "recently_played", sizeof(query) - 1);
        query[sizeof(query) - 1] = '\0';
    }
    char *buf = malloc(ACTIVITY_JSON_BUF_SZ);
    if (!buf) {
        const char *e = "{\"rows\":[],\"error\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_ACTIVITY_DB_QUERY_ACK, e, strlen(e));
    }
    size_t written = 0;
    activity_db_query_json(query, buf, ACTIVITY_JSON_BUF_SZ, &written);
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_ACTIVITY_DB_QUERY_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── SDK Changer handlers ───────────────────────────────────────────── */
#define SDK_JSON_BUF_SZ  (64u * 1024u)

static int handle_sdk_scan(runtime_state_t *state) {
    if (!state) return -1;
    char *buf = malloc(SDK_JSON_BUF_SZ);
    if (!buf) {
        const char *e = "{\"titles\":[]}";
        return mgmt_reply(MGMT_FRAME_SDK_SCAN_ACK, e, strlen(e));
    }
    size_t written = 0;
    sdk_changer_scan(buf, SDK_JSON_BUF_SZ, &written);
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_SDK_SCAN_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_sdk_patch(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    if (!body) {
        const char *e = "{\"ok\":false,\"error\":\"body_required\"}";
        return mgmt_reply(MGMT_FRAME_SDK_PATCH_ACK, e, strlen(e));
    }
    char title_id[32] = {0};
    char target_sdk[32] = {0};
    extract_json_string_field(body, "title_id", title_id, sizeof(title_id));
    extract_json_string_field(body, "target_sdk", target_sdk, sizeof(target_sdk));

    if (!title_id[0] || !target_sdk[0]) {
        const char *e = "{\"ok\":false,\"error\":\"title_id_and_target_sdk_required\"}";
        return mgmt_reply(MGMT_FRAME_SDK_PATCH_ACK, e, strlen(e));
    }

    char err[256] = {0};
    /* Optional, default off: the libc.prx symbol swap helps some titles and
     * breaks others, so it is never applied unless the caller asks. */
    int patch_libc_flag = 0;
    (void)extract_json_bool_field(body, "patch_libc", &patch_libc_flag);
    int rc = sdk_changer_patch(title_id, target_sdk, patch_libc_flag, err,
                               sizeof(err));
    cheat_inc_cmd_count(state);

    char title_id_esc[64];
    char target_sdk_esc[64];
    char err_esc[768];
    json_escape_into(title_id, title_id_esc, sizeof(title_id_esc));
    json_escape_into(target_sdk, target_sdk_esc, sizeof(target_sdk_esc));
    json_escape_into(err, err_esc, sizeof(err_esc));

    char resp[1024];
    if (rc == 0) {
        /* `err` carries the per-site counts on success — the UI shows
         * them so "patched" is verifiable rather than asserted. */
        snprintf(resp, sizeof(resp),
                 "{\"ok\":true,\"title_id\":\"%s\",\"target_sdk\":\"%s\","
                 "\"detail\":\"%s\"}",
                 title_id_esc, target_sdk_esc, err_esc);
    } else {
        snprintf(resp, sizeof(resp), "{\"ok\":false,\"error\":\"%s\"}",
                 err[0] ? err_esc : "patch_failed");
    }
    return mgmt_reply(MGMT_FRAME_SDK_PATCH_ACK, resp, strlen(resp));
}

static int handle_sdk_restore(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    if (!body) {
        const char *e = "{\"ok\":false,\"error\":\"body_required\"}";
        return mgmt_reply(MGMT_FRAME_SDK_RESTORE_ACK, e, strlen(e));
    }
    char title_id[32] = {0};
    extract_json_string_field(body, "title_id", title_id, sizeof(title_id));

    if (!title_id[0]) {
        const char *e = "{\"ok\":false,\"error\":\"title_id_required\"}";
        return mgmt_reply(MGMT_FRAME_SDK_RESTORE_ACK, e, strlen(e));
    }

    char err[256] = {0};
    int restored = 0;
    int rc = sdk_changer_restore(title_id, &restored, err, sizeof(err));
    cheat_inc_cmd_count(state);

    char title_id_esc[64];
    char err_esc[768];
    json_escape_into(title_id, title_id_esc, sizeof(title_id_esc));
    json_escape_into(err, err_esc, sizeof(err_esc));

    char resp[1024];
    if (rc == 0) {
        snprintf(resp, sizeof(resp),
                 "{\"ok\":true,\"title_id\":\"%s\",\"restored\":%d,\"error\":\"%s\"}",
                 title_id_esc, restored,
                 (rc == 0 && restored > 0) ? "" : (err[0] ? err_esc : ""));
    } else {
        snprintf(resp, sizeof(resp), "{\"ok\":false,\"error\":\"%s\"}",
                 err[0] ? err_esc : "restore_failed");
    }
    return mgmt_reply(MGMT_FRAME_SDK_RESTORE_ACK, resp, strlen(resp));
}

/* ── TMDB handlers ─────────────────────────────────────────────────── */
#define TMDB_JSON_BUF_SZ  (64u * 1024u)

static int handle_tmdb_fetch(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char title_id[32] = {0};
    int refresh = 0;
    if (body) {
        extract_json_string_field(body, "title_id", title_id, sizeof(title_id));
        extract_json_bool_field(body, "refresh", &refresh);
    }
    if (!title_id[0]) {
        const char *e = "{\"ok\":false,\"error\":\"title_id_required\"}";
        return mgmt_reply(MGMT_FRAME_TMDB_FETCH_ACK, e, strlen(e));
    }
    char *buf = malloc(TMDB_JSON_BUF_SZ);
    if (!buf) {
        const char *e = "{\"ok\":false,\"error\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_TMDB_FETCH_ACK, e, strlen(e));
    }
    size_t written = 0;
    tmdb_fetch(title_id, refresh, buf, TMDB_JSON_BUF_SZ, &written);
    cheat_inc_cmd_count(state);
    int rc = mgmt_reply(MGMT_FRAME_TMDB_FETCH_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

static int handle_tmdb_store(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    char title_id[32] = {0};
    char *json = malloc(TMDB_JSON_BUF_SZ);
    if (!json) {
        const char *e = "{\"ok\":false,\"error\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_TMDB_STORE_ACK, e, strlen(e));
    }
    size_t json_len = 0;
    if (body) {
        extract_json_string_field(body, "title_id", title_id, sizeof(title_id));
        extract_json_string_field(body, "json", json, TMDB_JSON_BUF_SZ);
        json_len = strlen(json);
    }
    if (!title_id[0] || json_len == 0) {
        free(json);
        const char *e = "{\"ok\":false,\"error\":\"title_id_and_json_required\"}";
        return mgmt_reply(MGMT_FRAME_TMDB_STORE_ACK, e, strlen(e));
    }
    int rc_store = tmdb_store(title_id, json, json_len);
    free(json);
    const char *resp = rc_store == 0
        ? "{\"ok\":true}" : "{\"ok\":false,\"error\":\"store_failed\"}";
    return mgmt_reply(MGMT_FRAME_TMDB_STORE_ACK, resp, strlen(resp));
}

/* ── FW Spoof status handler ───────────────────────────────────────── */
static int handle_fw_spoof_status(runtime_state_t *state) {
    if (!state) return -1;
    char buf[1024];
    size_t written = 0;
    fw_spoof_status(buf, sizeof(buf), &written);
    cheat_inc_cmd_count(state);
    return mgmt_reply(MGMT_FRAME_FWSPOOF_STATUS_ACK, buf, (uint64_t)written);
}

/* ── FTP Server handlers ──────────────────────────────────────────── */
static int handle_ftp_start(runtime_state_t *state, const char *body) {
    if (!state) return -1;
    int port = 2122;
    char root[256] = "/";
    int readonly = 0;
    char user[64] = {0};
    char pass[64] = {0};
    if (body) {
        char needle[32];
        snprintf(needle, sizeof(needle), "\"port\":");
        if (strstr(body, needle)) {
            port = (int)extract_json_uint64_field(body, "port");
        }
        extract_json_string_field(body, "root", root, sizeof(root));
        extract_json_bool_field(body, "readonly", &readonly);
        extract_json_string_field(body, "user", user, sizeof(user));
        extract_json_string_field(body, "pass", pass, sizeof(pass));
    }
    char resp[512];
    size_t written = 0;
    ftp_server_start(port, root, readonly, user[0] ? user : NULL,
                     pass[0] ? pass : NULL, resp, sizeof(resp), &written);
    cheat_inc_cmd_count(state);
    return mgmt_reply(MGMT_FRAME_FTP_START_ACK, resp, (uint64_t)written);
}

static int handle_ftp_status(runtime_state_t *state) {
    if (!state) return -1;
    char resp[256];
    size_t written = 0;
    ftp_server_status(resp, sizeof(resp), &written);
    cheat_inc_cmd_count(state);
    return mgmt_reply(MGMT_FRAME_FTP_STATUS_ACK, resp, (uint64_t)written);
}

/* ── Fan curve get handler ───────────────────────────────────────────── */
static int handle_fan_curve_get(runtime_state_t *state) {
    if (!state) return -1;
    char buf[4096];
    int rc = fan_curve_get(buf, sizeof(buf));
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    if (rc != 0) {
        const char *e = "{\"points\":[]}";
        return mgmt_reply(MGMT_FRAME_HW_FAN_CURVE_GET_ACK, e, strlen(e));
    }
    return mgmt_reply(MGMT_FRAME_HW_FAN_CURVE_GET_ACK, buf, strlen(buf));
}

/* The save / screenshot / video lists are built in one heap buffer of LIST_JSON_CAP bytes and
 * cut at an entry when it fills (the walkers stop at cap-2300/-2800). Over AVA1 the dispatcher
 * pages the array (`more`), so the buffer only bounds one walk. A walk that reached the limit
 * says so, with `"truncated":true`, instead of looking complete: 512 KiB is ~3,000 entries. */
#define LIST_JSON_CAP (512 * 1024)
#define LIST_JSON_TRUNC_MARGIN 2900

static int list_json_close(char *resp, int n, int cap) {
    if (n >= cap - LIST_JSON_TRUNC_MARGIN) {
        static const char t[] = "],\"truncated\":true}";
        if (n + (int)sizeof t <= cap) {
            memcpy(resp + n, t, sizeof t - 1);
            n += (int)sizeof t - 1;
        }
    } else if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    return n;
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

static int handle_list_saves(runtime_state_t *state, const char *body, uint64_t body_len) {
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
    char *resp = malloc(LIST_JSON_CAP);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_LIST_SAVES_ACK, err, strlen(err));
    }
    int cap = LIST_JSON_CAP;
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
    n = list_json_close(resp, n, cap);
    int rc = mgmt_reply(MGMT_FRAME_LIST_SAVES_ACK, resp, (uint64_t)n);
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
    /* Name this thread: the process listing shows a representative
     * thread that is not reliably main, so an unnamed worker makes the
     * whole process read as "payload.elf". See proc_identity.h. */
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    index_thread_args_t *args = (index_thread_args_t *)arg;
    for (int i = 0; i < args->root_count; i++) {
        index_walk(args->roots[i], 0);
    }
    pthread_mutex_lock(&g_index_lock);
    g_index_phase = 2;
    g_index_completed_at = time(NULL);
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

static int handle_index_start(runtime_state_t *state, const char *body, uint64_t body_len) {
    if (!state) return -1;
    pthread_mutex_lock(&g_index_lock);
    if (g_index_phase == 1) {
        pthread_mutex_unlock(&g_index_lock);
        const char *err = "{\"started\":false,\"err\":\"already_building\"}";
        return mgmt_reply(MGMT_FRAME_INDEX_START_ACK, err, strlen(err));
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
        return mgmt_reply(MGMT_FRAME_INDEX_START_ACK, err, strlen(err));
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
    if (pthread_create(&g_index_thread, NULL, index_thread_fn, args) != 0) {
        free(args);
        pthread_mutex_lock(&g_index_lock);
        g_index_phase = 0;
        pthread_mutex_unlock(&g_index_lock);
        const char *err = "{\"started\":false,\"err\":\"thread_create\"}";
        return mgmt_reply(MGMT_FRAME_INDEX_START_ACK, err, strlen(err));
    }
    pthread_detach(g_index_thread);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *ok = "{\"started\":true}";
    return mgmt_reply(MGMT_FRAME_INDEX_START_ACK, ok, strlen(ok));
}

static int handle_index_status(runtime_state_t *state) {
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
    return mgmt_reply(MGMT_FRAME_INDEX_STATUS_ACK, body, (uint64_t)n);
}

static int handle_search_index(runtime_state_t *state, const char *body, uint64_t body_len) {
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
        return mgmt_reply(MGMT_FRAME_SEARCH_INDEX_ACK, err, strlen(err));
    }
    int cap = 256 * 1024;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"results\":[");
    int wrote_one = 0;
    int matched = 0;
    int cut = 0;
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
        if (n >= cap - 2300) {
            cut = 1; /* the reply buffer is full: say so below, never look complete */
            break;
        }
        if (wrote_one) resp[n++] = ',';
        wrote_one = 1;
        n += snprintf(resp + n, cap - n,
                      "{\"path\":\"%s\",\"size\":%lld}",
                      esc_path, sz);
        matched++;
    }
    pthread_mutex_unlock(&g_index_lock);
    if (cut) {
        n += snprintf(resp + n, cap - n, "],\"truncated\":true}");
    } else if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc = mgmt_reply(MGMT_FRAME_SEARCH_INDEX_ACK, resp, (uint64_t)n);
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

static int handle_app_lifecycle(runtime_state_t *state, const char *body,
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
        return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, err, strlen(err));
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
            return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, err, strlen(err));
        }
        int count = 0;
        int rc = -1;
        /* sceApplication* is Sony code: serialised like every other Sony call
         * (P3 Task 6: AVA1 runs up to eight management calls at once, the old protocol ran one
         * thread per connection, and an unserialised Sony call can take the host
         * process down, CE-108262-9). */
        pthread_mutex_lock(&sony_api_lock);
        if (resolve_sce_syscore() == 0 && p_sceApplicationGetProcs) {
            rc = p_sceApplicationGetProcs(buf, APP_PROCS_MAX_COUNT, &count);
        }
        pthread_mutex_unlock(&sony_api_lock);
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
        int sret = mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, resp, (uint64_t)n);
        free(buf);
        free(resp);
        return sret;
    }
    if (app_id == 0) {
        const char *err = "{\"ok\":false,\"err\":\"app_id_required\"}";
        return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, err, strlen(err));
    }
    /* The suspend / resume / kill calls below are Sony APIs: hold sony_api_lock across
     * the whole action (the kill ladder is up to four calls, run back to back as before)
     * and release it on every exit. */
    pthread_mutex_lock(&sony_api_lock);
    if (resolve_sce_syscore() != 0) {
        pthread_mutex_unlock(&sony_api_lock);
        const char *err = "{\"ok\":false,\"err\":\"libSceSysCore_unavailable\"}";
        return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, err, strlen(err));
    }
    int rc = -1;
    if (strcmp(action, "suspend") == 0) {
        rc = p_sceApplicationSuspend ? p_sceApplicationSuspend(app_id) : -1;
    } else if (strcmp(action, "resume") == 0) {
        rc = p_sceApplicationResume ? p_sceApplicationResume(app_id) : -1;
    } else if (strcmp(action, "kill") == 0) {
        /* Kill ladder, gentlest first. libSceSysCore's sceApplicationKill is
         * tried first for firmwares where it works, but it is blind on 9.60
         * (empty GetProcs, 0x80AA0004), so LncUtil — the launcher util that
         * started the title — is the real graceful path. ForceKill is the last
         * resort before the client falls back to SIGKILL on the pid.
         *
         * Each rung is reported so a failure says WHICH api refused and with
         * what code, instead of a bare ok=false. */
        resolve_lnc_kill();
        rc = p_sceApplicationKill ? p_sceApplicationKill(app_id) : -1;
        if (rc != 0 && p_sceLncUtilKillApp) {
            int rc2 = p_sceLncUtilKillApp(app_id);
            fprintf(stderr,
                    "[payload2] kill app_id=%u: sceApplicationKill=0x%08x "
                    "sceLncUtilKillApp=0x%08x\n",
                    app_id, (unsigned)rc, (unsigned)rc2);
            rc = rc2;
        }
        if (rc != 0 && p_sceSystemServiceKillApp) {
            int rc3 = p_sceSystemServiceKillApp(app_id);
            fprintf(stderr,
                    "[payload2] kill app_id=%u: sceSystemServiceKillApp=0x%08x\n",
                    app_id, (unsigned)rc3);
            rc = rc3;
        }
        if (rc != 0 && p_sceLncUtilForceKillApp) {
            int rc4 = p_sceLncUtilForceKillApp(app_id);
            fprintf(stderr,
                    "[payload2] kill app_id=%u: sceLncUtilForceKillApp=0x%08x\n",
                    app_id, (unsigned)rc4);
            rc = rc4;
        }
    } else {
        pthread_mutex_unlock(&sony_api_lock);
        const char *err = "{\"ok\":false,\"err\":\"unknown_action\"}";
        return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, err, strlen(err));
    }
    pthread_mutex_unlock(&sony_api_lock);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[160];
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"action\":\"%s\",\"app_id\":%u,\"code\":%d}",
                     rc == 0 ? "true" : "false", action, app_id, rc);
    return mgmt_reply(MGMT_FRAME_APP_LIFECYCLE_ACK, resp, (uint64_t)n);
}

/* ── Kernel log read ─────────────────────────────────────────────────── */

/* Open /dev/klog once and read what's currently buffered. The kernel
 * log device returns 0 bytes when the buffer is empty (non-blocking
 * by default). We cap reads at 64 KB per call so the response stays
 * bounded. */
static int handle_klog_read(runtime_state_t *state, const char *body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, err, strlen(err));
    }
    char *buf = malloc(max_bytes);
    if (!buf) {
        close(fd);
        const char *err = "klog_oom";
        return mgmt_reply(MGMT_FRAME_ERROR, err, strlen(err));
    }
    ssize_t n = read(fd, buf, max_bytes);
    close(fd);
    if (n < 0) n = 0;
    int rc = mgmt_reply(MGMT_FRAME_KLOG_READ_ACK, buf, (uint64_t)n);
    free(buf);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* ── Network interface listing ───────────────────────────────────────── */

static int handle_net_interfaces(runtime_state_t *state) {
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
        return mgmt_reply(MGMT_FRAME_NET_INTERFACES_ACK, err, strlen(err));
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
            return mgmt_reply(MGMT_FRAME_NET_INTERFACES_ACK, err, strlen(err));
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
        return mgmt_reply(MGMT_FRAME_NET_INTERFACES_ACK, err, strlen(err));
    }
    n += snprintf(resp + n, resp_cap - n,
                  "],\"source\":\"%s\"}", source ? source : "unknown");
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    int rc = mgmt_reply(MGMT_FRAME_NET_INTERFACES_ACK, resp, (uint64_t)n);
    free(resp);
    return rc;
}

/* ── Peripheral control (BD/USB power) ───────────────────────────────── */

static int handle_peripheral_control(runtime_state_t *state, const char *body,
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
    /* Peripheral ICC control is serialised with the other Sony calls: AVA1 can run several management
     * calls at once, the old protocol rarely did. The lock covers only the control block; replies go out after. */
    int periph_known = strcmp(action, "bd_power_off") == 0 || strcmp(action, "bd_power_on") == 0 ||
                       strcmp(action, "eject_disc") == 0 || strcmp(action, "usb_port_off") == 0 ||
                       strcmp(action, "usb_port_on") == 0;
    if (periph_known) pthread_mutex_lock(&sony_api_lock);
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
        return mgmt_reply(MGMT_FRAME_PERIPHERAL_CONTROL_ACK, err, strlen(err));
    }
    if (periph_known) pthread_mutex_unlock(&sony_api_lock);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[160];
    int n = snprintf(resp, sizeof(resp),
                     "{\"ok\":%s,\"action\":\"%s\",\"port\":%d,\"code\":%d}",
                     rc == 0 ? "true" : "false", action, port, rc);
    return mgmt_reply(MGMT_FRAME_PERIPHERAL_CONTROL_ACK, resp, (uint64_t)n);
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
char *strdup_safe(const char *s) {
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
#include "shell_builtin.h"
static int shell_send_json_result(runtime_state_t *state, int exit_code,
                                  const char *stdout_text,
                                  const char *cwd_text,
                                  const char *session_id,
                                  int timed_out) {
    size_t out_len = stdout_text ? strlen(stdout_text) : 0;
    size_t cwd_len = cwd_text ? strlen(cwd_text) : 1;
    size_t sid_len = session_id ? strlen(session_id) : 0;

    /* Size for the ESCAPED length, not the raw length.
     *
     * This used to allocate raw length + 680 slack, which silently
     * truncated any output with more than a few hundred escapes: `\n`
     * costs two bytes and a control byte costs six, so `df` on a console
     * with ~1300 mounts overflowed the slack on newlines alone and
     * produced JSON that ended mid-string. One measuring pass is cheaper
     * than guessing. */
    size_t esc_len = 0;
    /* shell.exec over AVA1 answers at most 32 KiB (the plan's bound; the whole reply, JSON included):
     * the output is cut at a character boundary and the reply says so ("truncated":true). */
    const size_t shell_budget = 32u * 1024u - 2048u;
    int shell_truncated = 0;
    const int shell_capped = mgmt_capture_active();
    for (size_t i = 0; i < out_len; i++) {
        unsigned char c = (unsigned char)stdout_text[i];
        size_t w = (c == '\\' || c == '"' || c == '\n' || c == '\r' || c == '\t') ? 2 : (c < 0x20 ? 6 : 1);
        if (shell_capped && esc_len + w > shell_budget) {
            /* do not end inside a UTF-8 sequence */
            while (i > 0 && ((unsigned char)stdout_text[i] & 0xC0u) == 0x80u) {
                i--;
                esc_len -= 1;
            }
            out_len = i;
            shell_truncated = 1;
            break;
        }
        esc_len += w;
    }
    char *resp = malloc(esc_len + cwd_len * 6 + sid_len * 6 + 680);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_SHELL_EXEC_ACK, err, strlen(err));
    }
    int n = 0;
    size_t cap = esc_len + cwd_len * 6 + sid_len * 6 + 680;
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
    n += snprintf(resp + n, cap - (size_t)n, shell_truncated ? "\",\"truncated\":true}" : "\"}");
    int rc = mgmt_reply(MGMT_FRAME_SHELL_EXEC_ACK, resp, (uint64_t)n);
    free(resp);
    if (state) {
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);
    }
    return rc;
}

static int handle_shell_exec(runtime_state_t *state, const char *body,
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
        return mgmt_reply(MGMT_FRAME_SHELL_EXEC_ACK, err, strlen(err));
    }
    {
        char probe[2100];
        char *argv_probe[8];
        snprintf(probe, sizeof(probe), "%s", cmd);
        int argc_probe = shell_split(probe, argv_probe, 8);
        if (argc_probe > 0 && strcmp(argv_probe[0], "pwd") == 0) {
            char out[1100];
            snprintf(out, sizeof(out), "%s\n", cwd);
            return shell_send_json_result(state, 0, out, cwd, session_id, 0);
        }
        if (argc_probe > 0 && strcmp(argv_probe[0], "cd") == 0) {
            const char *target = argc_probe >= 2 ? argv_probe[1] : "/";
            char resolved[1024];
            char err[160];
            if (shell_resolve_dir(cwd, target, resolved, sizeof(resolved),
                                  err, sizeof(err)) != 0) {
                char out[1400];
                snprintf(out, sizeof(out), "cd: %s: %s\n", target, err);
                return shell_send_json_result(state, 1, out, cwd, session_id, 0);
            }
            shell_session_set(session_id, resolved);
            snprintf(cwd, sizeof(cwd), "%s", resolved);
            return shell_send_json_result(state, 0, "", cwd, session_id, 0);
        }
        if (argc_probe > 0 && strcmp(argv_probe[0], "ls") == 0 && argc_probe <= 2) {
            const char *target = argc_probe >= 2 ? argv_probe[1] : cwd;
            char abs_path[1024];
            char err[160];
            if (shell_resolve_dir(cwd, target, abs_path, sizeof(abs_path),
                                  err, sizeof(err)) != 0) {
                char out[1400];
                snprintf(out, sizeof(out), "ls: %s: %s\n", target, err);
                return shell_send_json_result(state, 1, out, cwd, session_id, 0);
            }
            char *builtin_out = NULL;
            int builtin_exit = -1;
            int builtin_rc = shell_ls_path(abs_path, &builtin_out, &builtin_exit);
            if (builtin_rc == 0) {
                int rc = shell_send_json_result(state, builtin_exit,
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

        /* chdir is process-global; the guard and its lock live with the
         * shell module rather than being reached across from here. */
        builtin_rc = shell_run_in_cwd(cwd, cmd, &builtin_out, &builtin_exit);

        if (builtin_rc == 0) {
            (void)timeout_secs;
            int rc_b = shell_send_json_result(state, builtin_exit,
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
    return shell_send_json_result(state, 127, msg, cwd, session_id, 0);
}

/* ── app.db query ────────────────────────────────────────────────────── */

/*
 * Installed titles, straight out of app.db.
 *
 * This used to resolve sqlite3_* through dlsym(RTLD_DEFAULT) and fall back
 * to a byte-level b-tree scan when the lookups came back NULL. They always
 * came back NULL -- no firmware ships libSceSqlite.sprx -- so the SQL half
 * was dead code on every console and the scan did all the work.
 *
 * The payload now links its own SQLite, so the SQL path is the real one.
 * The scan survives inside content_db_apps_json as the fallback for a
 * database that will not open at all (locked by the shell, or damaged),
 * which is a genuine runtime condition rather than a permanent one.
 */
static int handle_appdb_query(runtime_state_t *state) {
    if (!state) return -1;

    const size_t cap = 64 * 1024;
    char *resp = (char *)malloc(cap);
    if (!resp) {
        const char *err = "{\"err\":\"oom\",\"apps\":[]}";
        return mgmt_reply(MGMT_FRAME_APPDB_QUERY_ACK, err, strlen(err));
    }

    size_t written = 0;
    int rc;
    if (content_db_apps_json(resp, cap, &written) != 0 || written == 0) {
        const char *err = "{\"err\":\"appdb_unreadable\",\"apps\":[]}";
        rc = mgmt_reply(MGMT_FRAME_APPDB_QUERY_ACK, err, strlen(err));
    } else {
        rc = mgmt_reply(MGMT_FRAME_APPDB_QUERY_ACK, resp, (uint64_t)written);
    }
    free(resp);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* ── appinfo.db ──────────────────────────────────────────────────────── */

/* Defined further down with the .pkg mount handlers; declared here because
 * these two handlers sit next to the app.db handler they belong with. */
static int parse_json_string_field_local(const char *body, uint64_t body_len,
                                          const char *field, char *out,
                                          size_t out_size);


static int handle_appinfo_query(runtime_state_t *state, const char *body,
                                 uint64_t body_len) {
    if (!state) return -1;
    char title_id[32] = {0};
    char keys[256] = {0};
    (void)parse_json_string_field_local(body, body_len, "keys", keys,
                                        sizeof(keys));

    int rc;
    if (parse_json_string_field_local(body, body_len, "title_id", title_id,
                                       sizeof(title_id)) != 0 ||
        title_id[0] == '\0') {
        const char *err =
            "{\"ok\":false,\"error\":\"title_id is required\"}";
        rc = mgmt_reply(MGMT_FRAME_APPINFO_QUERY_ACK, err, strlen(err));
    } else {
        const size_t cap = 32 * 1024;
        char *resp = (char *)malloc(cap);
        if (!resp) {
            const char *err = "{\"ok\":false,\"error\":\"oom\"}";
            rc = mgmt_reply(MGMT_FRAME_APPINFO_QUERY_ACK, err, strlen(err));
        } else {
            size_t written = 0;
            if (content_db_appinfo_json(title_id, keys[0] ? keys : NULL, resp,
                                        cap, &written) != 0 || written == 0) {
                const char *err =
                    "{\"ok\":false,\"error\":\"appinfo.db unreadable\"}";
                rc = mgmt_reply(MGMT_FRAME_APPINFO_QUERY_ACK, err, strlen(err));
            } else {
                rc = mgmt_reply(MGMT_FRAME_APPINFO_QUERY_ACK, resp, (uint64_t)written);
            }
            free(resp);
        }
    }

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

static int handle_appinfo_set(runtime_state_t *state, const char *body,
                               uint64_t body_len) {
    if (!state) return -1;
    char title_id[32] = {0};
    char key[128] = {0};
    char val[512] = {0};
    char resp[768];
    int n;

    if (parse_json_string_field_local(body, body_len, "title_id", title_id,
                                       sizeof(title_id)) != 0 ||
        parse_json_string_field_local(body, body_len, "key", key,
                                       sizeof(key)) != 0 ||
        parse_json_string_field_local(body, body_len, "val", val,
                                       sizeof(val)) != 0) {
        n = snprintf(resp, sizeof(resp),
                     "{\"ok\":false,\"err\":\"title_id, key and val are "
                     "required\"}");
    } else {
        char err[256] = {0};
        if (content_db_appinfo_set(title_id, key, val, err,
                                   sizeof(err)) == 0) {
            n = snprintf(resp, sizeof(resp), "{\"ok\":true,\"err\":null}");
            /* A Settings -> Storage edit is invisible until the user looks;
             * say so on the console so the change is never silent. */
            char toast[256];
            snprintf(toast, sizeof(toast), "%s: %s updated", title_id, key);
            pop_notification(toast);
        } else {
            char esc[512];
            json_escape_into(err, esc, sizeof(esc));
            n = snprintf(resp, sizeof(resp), "{\"ok\":false,\"err\":\"%s\"}",
                         esc);
        }
    }

    int rc = mgmt_reply(MGMT_FRAME_APPINFO_SET_ACK, resp, (uint64_t)(n > 0 ? n : 0));
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
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

static int handle_pkg_direct_mount(runtime_state_t *state, const char *body,
                                    uint64_t body_len) {
    if (!state) return -1;
    char pkg_path[512] = {0};
    char mount_point[256] = {0};
    if (parse_json_string_field_local(body, body_len, "pkg_path",
                                       pkg_path, sizeof(pkg_path)) != 0 ||
        pkg_path[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"pkg_path_required\"}";
        return mgmt_reply(MGMT_FRAME_PKG_DIRECT_MOUNT_ACK, err, strlen(err));
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
        return mgmt_reply(MGMT_FRAME_PKG_DIRECT_MOUNT_ACK, err, strlen(err));
    }
    /* Resolve the optional libSceFsInternalForVsh export at first use. */
    if (resolve_sce_fs_internal() != 0 || !p_sceFsMountGamePkg) {
        const char *err = "{\"ok\":false,\"err\":\"libSceFsInternalForVsh_unavailable\"}";
        return mgmt_reply(MGMT_FRAME_PKG_DIRECT_MOUNT_ACK, err, strlen(err));
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
    return mgmt_reply(MGMT_FRAME_PKG_DIRECT_MOUNT_ACK, resp, (uint64_t)n);
}

static int handle_ufs_fsck(runtime_state_t *state, const char *body,
                            uint64_t body_len) {
    if (!state) return -1;
    char device[256] = {0};
    if (parse_json_string_field_local(body, body_len, "device",
                                       device, sizeof(device)) != 0 ||
        device[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"device_required\"}";
        return mgmt_reply(MGMT_FRAME_UFS_FSCK_ACK, err, strlen(err));
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
            return mgmt_reply(MGMT_FRAME_UFS_FSCK_ACK, err, strlen(err));
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
        return mgmt_reply(MGMT_FRAME_UFS_FSCK_ACK, err, strlen(err));
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
    return mgmt_reply(MGMT_FRAME_UFS_FSCK_ACK, resp, (uint64_t)n);
}

static int handle_lwfs_mount(runtime_state_t *state, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    char patch_path[512] = {0};
    char mount_point[256] = {0};
    char title_id[64] = {0};
    if (parse_json_string_field_local(body, body_len, "patch_path",
                                       patch_path, sizeof(patch_path)) != 0 ||
        patch_path[0] == '\0') {
        const char *err = "{\"ok\":false,\"err\":\"patch_path_required\"}";
        return mgmt_reply(MGMT_FRAME_LWFS_MOUNT_ACK, err, strlen(err));
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
        return mgmt_reply(MGMT_FRAME_LWFS_MOUNT_ACK, err, strlen(err));
    }
    if (resolve_sce_fs_internal() != 0 || !p_sceFsMountLwfs) {
        const char *err = "{\"ok\":false,\"err\":\"libSceFsInternalForVsh_unavailable\"}";
        return mgmt_reply(MGMT_FRAME_LWFS_MOUNT_ACK, err, strlen(err));
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
    return mgmt_reply(MGMT_FRAME_LWFS_MOUNT_ACK, resp, (uint64_t)n);
}

/* ── Reach-back probe ─────────────────────────────────────────────────── */

/* Body: {"host":"a.b.c.d","port":"19113","timeout_ms":"3000"} (numbers as
 * strings, so the one string-field parser covers them). Opens a TCP
 * connection from the console and closes it at once; nothing is sent. The
 * engine points it at its own pkg-host listener before a stream install. */
static int handle_net_reach(runtime_state_t *state, const char *body,
                            uint64_t body_len) {
    if (!state) return -1;
    char resp[320];
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    /* The probe itself lives in net_probe.c so the host tests run the real code. */
    size_t n = net_probe_reach(body, (size_t)body_len, resp, sizeof(resp));
    return mgmt_reply(MGMT_FRAME_NET_REACH_ACK, resp, (uint64_t)n);
}

/* ── Network round-trip ack ──────────────────────────────────────────── */

/* The "speed test" is observed entirely on the client side. The
 * client sends N empty-body NetSpeedTest frames; the payload just
 * acks each one cheaply. The client measures wall time around the
 * batch and per-frame round-trips. No state on the payload. */
static int handle_net_speed_test(runtime_state_t *state, const char *body,
                                  uint64_t body_len) {
    if (!state) return -1;
    (void)body;
    (void)body_len;
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    const char *resp = "{\"ok\":true}";
    return mgmt_reply(MGMT_FRAME_NET_SPEED_TEST_ACK, resp, strlen(resp));
}

/* ── Module enumeration ──────────────────────────────────────────────── */

static int handle_proc_modules(runtime_state_t *state, const char *body,
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
        return mgmt_reply(MGMT_FRAME_PROC_MODULES_ACK, err, strlen(err));
    }
    int cap = 64 * 1024;
    int n = 0;
    {
        int w = snprintf(resp + n, cap - n, "{\"modules\":[");
        if (w < 0 || w >= cap - n) { n = cap; } else { n += w; }
    }
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
        int w = snprintf(resp + n, n < cap ? (size_t)(cap - n) : 0,
                       "{\"handle\":%d,\"name\":\"%s\","
                       "\"base\":\"%p\",\"code_size\":%zu}",
                       handles[i], esc_name, info.base_addr, info.code_size);
        if (w < 0) break;
        if ((size_t)w >= (size_t)(cap - n)) { n = cap; break; }
        n += w;
    }
    if (n < cap - 2) {
        resp[n++] = ']';
        resp[n++] = '}';
    }
    int rc2 = mgmt_reply(MGMT_FRAME_PROC_MODULES_ACK, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc2;
}

/* ── Rich JSON toast (sceNotificationSend) ───────────────────────────── */

static int handle_toast_send(runtime_state_t *state, const char *body,
                              uint64_t body_len) {
    if (!state) return -1;
    /* Body is the JSON the renderer wants to forward. We don't try to
     * validate it — Sony's daemon parses the template; an invalid
     * shape just produces a silently-dropped notification. We do
     * size-cap to 4 KB to keep an over-eager renderer from streaming
     * huge bodies. */
    if (!body || body_len == 0 || body_len > 4096) {
        const char *err = "{\"ok\":false,\"err\":\"body_required\"}";
        return mgmt_reply(MGMT_FRAME_TOAST_SEND_ACK, err, strlen(err));
    }
    /* Need a null-terminated copy for sceNotificationSend. */
    char *json = malloc(body_len + 1);
    if (!json) {
        const char *err = "{\"ok\":false,\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_TOAST_SEND_ACK, err, strlen(err));
    }
    memcpy(json, body, body_len);
    json[body_len] = '\0';
    /* `target_user_id = -1` = broadcast to all logged-in users.
     * `flag = 0` (system-default formatting). Return is non-zero on
     * malformed JSON or daemon offline; we surface it for the
     * renderer to log but don't treat as fatal. */
    resolve_sce_notification();
    /* a Sony call: one at a time with the others (AVA1 runs up to 8 management calls at once) */
    pthread_mutex_lock(&sony_api_lock);
    int rc = p_sceNotificationSend
                ? p_sceNotificationSend(-1, 0, json)
                : -1; /* symbol missing on this FW: toast unavailable */
    pthread_mutex_unlock(&sony_api_lock);
    free(json);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    char resp[64];
    int n = snprintf(resp, sizeof(resp), "{\"ok\":%s,\"code\":%d}",
                     rc == 0 ? "true" : "false", rc);
    return mgmt_reply(MGMT_FRAME_TOAST_SEND_ACK, resp, (uint64_t)n);
}

static int handle_index_cancel(runtime_state_t *state) {
    if (!state) return -1;
    pthread_mutex_lock(&g_index_lock);
    g_index_cancel = 1;
    pthread_mutex_unlock(&g_index_lock);
    const char *ok = "{\"cancelled\":true}";
    return mgmt_reply(MGMT_FRAME_INDEX_CANCEL_ACK, ok, strlen(ok));
}

static int handle_list_screenshots(runtime_state_t *state) {
    if (!state) return -1;
    char *resp = malloc(LIST_JSON_CAP);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_LIST_SCREENSHOTS_ACK, err, strlen(err));
    }
    int cap = LIST_JSON_CAP;
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
    n = list_json_close(resp, n, cap);
    int rc = mgmt_reply(MGMT_FRAME_LIST_SCREENSHOTS_ACK, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

/* List gameplay video clips (parallels handle_list_screenshots). No
 * thumbnail tree to dedup — a single flat walk of av_contents/video. */
static int handle_list_videos(runtime_state_t *state) {
    if (!state) return -1;
    char *resp = malloc(LIST_JSON_CAP);
    if (!resp) {
        const char *err = "{\"err\":\"oom\"}";
        return mgmt_reply(MGMT_FRAME_LIST_VIDEOS_ACK, err, strlen(err));
    }
    int cap = LIST_JSON_CAP;
    int n = 0;
    n += snprintf(resp + n, cap - n, "{\"items\":[");
    int wrote_one = 0;
    walk_videos("/user/av_contents/video", 5, resp, &n, cap, &wrote_one);
    n = list_json_close(resp, n, cap);
    int rc = mgmt_reply(MGMT_FRAME_LIST_VIDEOS_ACK, resp, (uint64_t)n);
    free(resp);
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return rc;
}

static int handle_hw_storage(runtime_state_t *state) {
    return handle_hw_text_op(state, hw_storage_get_text,
                              MGMT_FRAME_HW_STORAGE_ACK, "hw_storage_failed");
}

static int handle_hw_drive_sensors(runtime_state_t *state) {
    return handle_hw_text_op(state, drive_sensors_get_json,
                              MGMT_FRAME_HW_DRIVE_SENSORS_ACK, "hw_drive_sensors_failed");
}

/* Parse body as "NN" (ASCII decimal). Accepts an empty-body shortcut
 * meaning "reset to default 65 °C" so future UI can send a zero-body
 * frame as a quick reset. Leading whitespace and a trailing newline
 * are tolerated because some shells (lab CLI, curl) add them. */
static int handle_hw_set_fan_threshold(runtime_state_t *state, const char *body, uint64_t body_len) {
    if (!state) return -1;

    uint8_t threshold = 65;  /* Sony's approximate default. */
    /* Optional reapply interval in seconds. If present in the body
     * (as the second integer), update the watcher's interval too.
     * Backward compat: a body with just the threshold ("65") leaves
     * the interval unchanged. */
    int has_reapply = 0;
    int reapply_sec = 0;
    if (body_len > 0 && body_len < 32) {
        char buf[32];
        memcpy(buf, body, (size_t)body_len);
        buf[body_len] = '\0';
        /* Parse "threshold" or "threshold reapply_sec". hw_fan_set_threshold
         * clamps the threshold so a non-numeric payload degrades to a
         * safe default rather than being rejected. */
        int parsed = 0;
        int second = 0;
        int matched = sscanf(buf, "%d %d", &parsed, &second);
        if (matched >= 1 && parsed > 0 && parsed < 255) {
            threshold = (uint8_t)parsed;
        }
        if (matched >= 2 && second > 0) {
            has_reapply = 1;
            reapply_sec = second;
        }
    } else if (body_len >= 32) {
        static const char err[] = "body_too_long";
        return mgmt_reply(MGMT_FRAME_ERROR, err, (uint64_t)(sizeof(err) - 1));
    }

    const char *err_reason = NULL;
    if (hw_fan_set_threshold(threshold, &err_reason) != 0) {
        const char *reason = err_reason ? err_reason : "fan_set_failed";
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    /* Update reapply interval if the caller provided one. Done AFTER
     * the threshold set so a bad interval value doesn't prevent the
     * threshold from being applied. hw_fan_set_reapply_interval
     * clamps to [1, 300] and persists to fan_reapply.conf. */
    if (has_reapply) {
        hw_fan_set_reapply_interval(reapply_sec);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_HW_SET_FAN_THRESHOLD_ACK, NULL, 0);
}

/* APP_LAUNCH_BROWSER: open the PS5 web browser. Implementation uses
 * sceSystemServiceLaunchApp with the known-stable NPXS browser title
 * id. Resolved via the same libSceSystemService handle the launch path
 * already uses. */
extern int   register_browser_launch(void);

static int handle_app_launch_browser(runtime_state_t *state) {
    if (!state) return -1;
    if (register_browser_launch() != 0) {
        static const char err[] = "launch_browser_unavailable";
        return mgmt_reply(MGMT_FRAME_ERROR, err, (uint64_t)(sizeof(err) - 1));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_APP_LAUNCH_BROWSER_ACK, NULL, 0);
}

/* PROC_LIST: walk allproc via kernel R/W and return a JSON array of
 * running processes. Read-only — we never touch process state here.
 * The body size is bounded so a system with a corrupt proc list can't
 * blow the buffer; see proc_list.c for the truncation policy. */
static int handle_proc_list(runtime_state_t *state, const char *request_body,
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
        return mgmt_reply(MGMT_FRAME_ERROR, "proc_list_oom", 13);
    }
    if (proc_list_get_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "proc_list_failed";
        rc = mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = mgmt_reply(MGMT_FRAME_PROC_LIST_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* PROCESS_LIST: the detailed process enumerate for the in-app process
 * manager — pid/name/comm/title_id/app_id/memory/threads/kind per process.
 * Same sysctl walk as PROC_LIST but the richer proc_list_get_json_ex body.
 * Read-only; no kernel write, no elevation needed for the enumerate. */
static int handle_process_list(runtime_state_t *state) {
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
        return mgmt_reply(MGMT_FRAME_ERROR, "process_list_oom", 16);
    }
    if (proc_list_get_json_ex(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "process_list_failed";
        rc = mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = mgmt_reply(MGMT_FRAME_PROCESS_LIST_ACK, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* FOCUS_PROBE: which application currently owns the screen.
 *
 * Read-only and dlsym-only. This deliberately does NOT ptrace SceShellUI:
 * a stopped-and-not-resumed ShellUI freezes the console UI hard enough to
 * need a power-button recovery, which this codebase has already caused
 * once. See focus_probe.h. */
static int handle_focus_probe(runtime_state_t *state) {
    /* Symbol map plus one row per running app. 8 KiB covers the candidate
     * table and a busy console's app list with wide headroom, and keeping it
     * on the stack keeps a 1 Hz poller cheap. */
    char buf[8192];
    size_t written = 0;
    const char *err = NULL;
    if (!state) return -1;
    if (focus_probe_get_json(buf, sizeof buf, &written, &err) != 0) {
        const char *reason = err ? err : "focus_probe_failed";
        return mgmt_reply(MGMT_FRAME_ERROR, reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return mgmt_reply(MGMT_FRAME_FOCUS_PROBE_ACK, buf, (uint64_t)written);
}

/* PROCESS_KILL: SIGKILL the pid in the request body ({"pid":N}). proc_kill
 * guards self/kernel/init; the UI is responsible for warning before a
 * "system" kill. Ack {"ok":bool,"pid":N[,"err":"..."]}. */
static int handle_process_kill(runtime_state_t *state, const char *body,
                               uint64_t body_len) {
    (void)body_len;
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
        return mgmt_reply(MGMT_FRAME_PROCESS_KILL_ACK, e, strlen(e));
    }
    return mgmt_reply(MGMT_FRAME_PROCESS_KILL_ACK, ack, (size_t)n);
}

/* SYSLOG_TAIL: return the PS5 kernel-log circular buffer (dmesg
 * equivalent). Read via `sysctl kern.msgbuf` — the kernel's in-memory
 * printk/printf history. Used by the desktop's "PS5 system log" panel to
 * surface "why did the payload not load / why is X silently failing"
 * without making the user FTP / ssh in. 64 KiB cap is well above the
 * default PS5 msgbuf size; if the kernel was rebuilt with a smaller
 * buffer the sysctl just returns less and we ack the actual length.
 */
static int handle_syslog_tail(runtime_state_t *state) {
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
        return mgmt_reply(MGMT_FRAME_ERROR, reason, rn > 0 ? (uint64_t)rn : 0);
    }
    if (needed == 0) {
        /* Empty msgbuf is a successful read of zero bytes. */
        return mgmt_reply(MGMT_FRAME_SYSLOG_TAIL_ACK, "", 0);
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
        return mgmt_reply(MGMT_FRAME_ERROR, "syslog_tail_oom", 15);
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
            return mgmt_reply(MGMT_FRAME_ERROR, "syslog_tail_oom", 15);
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
        return mgmt_reply(MGMT_FRAME_ERROR, reason, rn > 0 ? (uint64_t)rn : 0);
    }
    /* sz holds the actual byte count written by sysctl. The buffer is
     * plain text (kernel printf output) — let the client treat it as a
     * sized byte slice (no NUL-termination assumption). */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = mgmt_reply(MGMT_FRAME_SYSLOG_TAIL_ACK, buf, (uint64_t)sz);
    free(buf);
    return rc;
}

/* ── Profile (avatar + offline-account) frame handlers ──────────────────── */

static int handle_profile_info(void) {
    /* Serialize the sceRegMgr/sceUserService calls below: these Sony APIs
     * are not safe to call concurrently from multiple management workers
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
        return mgmt_reply(MGMT_FRAME_PROFILE_INFO_ACK, err, strlen(err));
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
        /* Activated means the account can be used — it has an id and an
         * "np" type. It does NOT mean "activated by us".
         *
         * PROFILE_DEFAULT_FLAGS (0x1002) is what our own offline
         * activation writes; a legitimately PSN-linked account carries
         * different flags entirely (6, on a real console). Requiring our
         * exact value told those users they were "not activated" — which
         * invites them to run activation on a perfectly good account, and
         * rewriting the account id of a working profile is how save data
         * gets orphaned. `offline` reports HOW it was activated, which is
         * the genuinely useful distinction. */
        int activated = (id != 0 && strcmp(type, "np") == 0);
        int offline_act = (id != 0 && flags == PROFILE_DEFAULT_FLAGS);
        int n = snprintf(body + len, sizeof(body) - len,
            "%s{\"slot\":%d,\"name\":\"%s\",\"type\":\"%s\",\"flags\":%d,"
            "\"id\":\"0x%016llx\",\"activated\":%s,\"offline_activated\":%s}",
            first ? "" : ",", s, nesc, tesc, flags,
            (unsigned long long)id, activated ? "true" : "false",
            offline_act ? "true" : "false");
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
    return mgmt_reply(MGMT_FRAME_PROFILE_INFO_ACK, body, (uint64_t)len);
}

static int handle_profile_set_username(const char *body) {
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
    return mgmt_reply(MGMT_FRAME_PROFILE_SET_USERNAME_ACK, resp, (uint64_t)len);
}

static int handle_profile_activate(const char *body) {
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
    return mgmt_reply(MGMT_FRAME_PROFILE_ACTIVATE_ACK, resp, (uint64_t)len);
}

static int handle_profile_clear_slot(const char *body) {
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
    return mgmt_reply(MGMT_FRAME_PROFILE_CLEAR_SLOT_ACK, resp, (uint64_t)len);
}

static int handle_profile_apply_avatar(const char *body) {
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
    return mgmt_reply(MGMT_FRAME_PROFILE_APPLY_AVATAR_ACK, resp, (uint64_t)len);
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

static int handle_profile_set_local_username(const char *body) {
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
    return mgmt_reply(MGMT_FRAME_PROFILE_SET_LOCAL_USERNAME_ACK, resp, (uint64_t)len);
}

static int handle_status_frame(runtime_state_t *state) {
    /* Snapshot cross-thread fields under the mutex; keep the lock window to memory copies only. */
    uint64_t snap_instance_id, snap_started_at, snap_command_count;
    int snap_shutdown, snap_startup_reason, snap_takeover_req;
    int snap_prior_verdict;
    int len;
    char body[2048];
    const size_t body_cap = sizeof body;
    char kernel_version_raw[256];
    char kernel_version_esc[512];
    read_ps5_kernel_version(kernel_version_raw, sizeof(kernel_version_raw));
    json_escape_into(kernel_version_raw, kernel_version_esc, sizeof(kernel_version_esc));
    pthread_mutex_lock(&state->state_mtx);
    snap_instance_id    = state->instance_id;
    snap_shutdown       = state->shutdown_requested;
    snap_startup_reason = state->startup_reason;
    snap_prior_verdict  = state->prior_verdict;
    snap_takeover_req   = state->takeover_requested;
    snap_started_at     = state->started_at_unix;
    snap_command_count  = state->command_count;
    pthread_mutex_unlock(&state->state_mtx);
    /* Surface ucred elevation result so the client UI can warn
     * "load kstuff first" when elevation == false without
     * having to call a Sony API that might wedge. The pid -1
     * value used in main()'s call means "current process";
     * 0 from the kernel = elevation succeeded.
     * `g_ucred_elevation_rc` is defined in main.c. */
    extern volatile int g_ucred_elevation_rc;
    const int ucred_elevated = (g_ucred_elevation_rc == 0) ? 1 : 0;
    /* Fan threshold + reapply interval for the client UI. Both are
     * read from atomics in hw_info.c so this is lock-free. */
    int fan_pinned = hw_fan_pinned_threshold();
    int fan_reapply = hw_fan_reapply_interval();
    len = snprintf(body, body_cap,
                   "{\"version\":\"%s\","
                   "\"ps5_kernel\":\"%s\","
                   /* runtime_port stays for one release as 0 (there is no helper port but the AVA1 one) so an
                    * older client UI that still reads it does not break; the current client ignores it. */
                   "\"instance_id\":%llu,\"runtime_port\":0,"
                   "\"shutdown\":%d,\"startup_reason\":%d,"
                   "\"takeover_requested\":%d,\"started_at_unix\":%llu,"
                   /* How the PREVIOUS instance ended: "clean",
                    * "killed_externally", "wedged" or "stale". Absent on
                    * older payloads — the client treats that as unknown. */
                   "\"prior_instance\":\"%s\","
                   "\"command_count\":%llu,"
                   "\"ucred_elevated\":%s,"
                   /* Also kept for one release: the most data lanes one session can open (the AVA1 lane maximum). */
                   "\"max_transfer_streams\":%d,"
                   /* Fan state: pinned threshold (0 = not set) and the
                    * reapply interval in seconds. Lets the client display
                    * current settings without a separate round-trip. */
                   "\"fan_threshold\":%d,"
                   "\"fan_reapply_sec\":%d}",
                   PS5UPLOAD2_VERSION,
                   kernel_version_esc,
                   (unsigned long long)snap_instance_id,
                   snap_shutdown,
                   snap_startup_reason,
                   snap_takeover_req,
                   (unsigned long long)snap_started_at,
                   instance_verdict_name(
                       (ps5upload2_prior_verdict_t)snap_prior_verdict),
                   (unsigned long long)snap_command_count,
                   ucred_elevated ? "true" : "false",
                   (int)AVA1_MAX_LANES,
                   fan_pinned,
                   fan_reapply);
    /* Truncation-safe: if the fields ever grow past `body`, clamp rather than emit a body_len that
     * reads past the stack buffer. The node.status runner then fails to parse it, which is the right
     * failure mode. */
    if (len < 0) return -1;
    if ((size_t)len >= body_cap) len = (int)(body_cap - 1);
    return mgmt_reply(MGMT_FRAME_STATUS_ACK, body, (uint64_t)len);
}


__thread volatile unsigned int g_inflight_frame_type = 0;

/* node.shutdown (the old shutdown frame's body, a handler of its own for the AVA1 table). */
/* The native filesystem methods (mgmt_fs.c) take their policy from here: the same allowlist, the same
 * read carve-outs and the same command counter the management handlers use. */
static runtime_state_t *g_mgmt_fs_state;

static int mgmt_fs_read_allowed(const char *path, int unsafe_read) {
    return is_path_allowed(path) || is_profile_avatar_read_path(path) ||
           (unsafe_read && is_safe_unsafe_read_path(path));
}

static void mgmt_fs_count(void) {
    runtime_state_t *st = g_mgmt_fs_state;
    if (!st) return;
    pthread_mutex_lock(&st->state_mtx);
    st->command_count += 1;
    pthread_mutex_unlock(&st->state_mtx);
}

/* ---- P3 Task 7 table adapters ----
 * mgmt_table.def calls a body handler as (state, body, len). These handlers take a different
 * shape (no length, or no state), so a one-line adapter gives them the table's. Each adapter only
 * forwards the arguments; the body arrives NUL-terminated (mgmt_legacy_call). */
static int mgmt_w_fan_curve_set(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_fan_curve_set(st, b);
}
static int mgmt_w_user_create(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_user_create(st, b);
}
static int mgmt_w_user_delete(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_user_delete(st, b);
}
static int mgmt_w_backup_list(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_backup_list(st, b);
}
static int mgmt_w_backup_delete(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_backup_delete(st, b);
}
static int mgmt_w_remoteplay_request(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_remoteplay_request(st, b);
}
static int mgmt_w_remoteplay_enable(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_remoteplay_enable(st, b);
}
static int mgmt_w_activity_db_query(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_activity_db_query(st, b);
}
static int mgmt_w_notif_list(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_notif_list(st, b);
}
static int mgmt_w_cheats_get(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_cheats_get(st, b);
}
static int mgmt_w_cheats_toggle(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_cheats_toggle(st, b);
}
static int mgmt_w_cheats_delete(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_cheats_delete(st, b);
}
static int mgmt_w_cheats_engine_set(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_cheats_engine_set(st, b);
}
static int mgmt_w_sdk_patch(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_sdk_patch(st, b);
}
static int mgmt_w_sdk_restore(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_sdk_restore(st, b);
}
static int mgmt_w_tmdb_fetch(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_tmdb_fetch(st, b);
}
static int mgmt_w_tmdb_store(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_tmdb_store(st, b);
}
static int mgmt_w_ftp_start(runtime_state_t *st, const char *b, uint64_t l) {
    (void)l;
    return handle_ftp_start(st, b);
}
static int mgmt_w_profile_set_username(runtime_state_t *st, const char *b, uint64_t l) {
    (void)st;
    (void)l;
    return handle_profile_set_username(b);
}
static int mgmt_w_profile_activate(runtime_state_t *st, const char *b, uint64_t l) {
    (void)st;
    (void)l;
    return handle_profile_activate(b);
}
static int mgmt_w_profile_apply_avatar(runtime_state_t *st, const char *b, uint64_t l) {
    (void)st;
    (void)l;
    return handle_profile_apply_avatar(b);
}
static int mgmt_w_profile_clear_slot(runtime_state_t *st, const char *b, uint64_t l) {
    (void)st;
    (void)l;
    return handle_profile_clear_slot(b);
}
static int mgmt_w_profile_set_local_username(runtime_state_t *st, const char *b, uint64_t l) {
    (void)st;
    (void)l;
    return handle_profile_set_local_username(b);
}
static int mgmt_w_profile_info(runtime_state_t *st) {
    (void)st;
    return handle_profile_info();
}

/* notif.send shares toast.send's 4 KiB request cap over AVA1. */
static int mgmt_w_notif_send(runtime_state_t *st, const char *b, uint64_t l) {
    if (l > 4096) return mgmt_reply(MGMT_FRAME_ERROR, "body_too_large", 14);
    return handle_notif_send(st, b);
}

/* toast.send: the old dispatcher refused a body over 4 KiB before the handler ran (and the handler
 * checks it again). Over AVA1 a larger body is ERR_PROTOCOL ("body_too_large"). */
static int mgmt_w_toast_send(runtime_state_t *st, const char *b, uint64_t l) {
    if (l > 4096) return mgmt_reply(MGMT_FRAME_ERROR, "body_too_large", 14);
    return handle_toast_send(st, b, l);
}

/* The AVA1 management table (mgmt_table.def) and its thread environment. */
/* Asks this instance to exit: node.shutdown and the takeover flag file land here. Sets the flag
 * main() waits on (runtime_wait_for_shutdown). Safe to call from any thread, more than once. */
static void shutdown_common(runtime_state_t *state, const char *why) {
    if (!state) return;
    fprintf(stderr, "[payload2] shutdown requested: %s\n", why ? why : "?");
    state->shutdown_requested = 1;
}

void runtime_request_shutdown(runtime_state_t *state, const char *why) {
    shutdown_common(state, why);
}

static void node_shutdown_fire(void *arg) { shutdown_common((runtime_state_t *)arg, "node_shutdown"); }

/* node.shutdown (AVA1 method 5). The handler runs under the capture sink: its reply leaves only after
 * it returns. So the exit is DEFERRED to a short thread (like power.control): the caller receives the
 * acknowledgement first, then the instance stops (main.c runs ava1_payload_stop after the server loop). */
static int handle_node_shutdown(runtime_state_t *state) {
    int rc = mgmt_reply(MGMT_FRAME_SHUTDOWN_ACK, "{}", 2);
    if (ava1_shutdown_defer(300, node_shutdown_fire, state) != 0) node_shutdown_fire(state);
    return rc;
}

#include "mgmt_install.inc"

/* Blocks until something asked this instance to exit. The AVA1 server runs on its own threads; the
 * main thread has nothing else to serve. */
void runtime_wait_for_shutdown(runtime_state_t *state) {
    if (!state) return;
    while (!*(volatile int *)&state->shutdown_requested) usleep(100000);
}
