/*
 * Drive SMART / temperature sensors for ps5upload.
 *
 * Enumerates /dev/daN block devices, reads temperature via SCSI LOG
 * SENSE (CAM pass-through, page 0x0D), and collects capacity, ident,
 * and filesystem usage. Ported from elf-arsenal's drive_sensors.c,
 * adapted to emit FTX2-compatible JSON instead of an HTTP response.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/disk.h>
#include <sys/statvfs.h>
#include <sys/param.h>
#include <sys/mount.h>

/* CAM/SCSI headers for LOG SENSE pass-through. These are available
 * in the PS5 SDK's FreeBSD 11 userland headers. */
#include <cam/cam.h>
#include <cam/cam_ccb.h>
#include <cam/scsi/scsi_all.h>
#include <cam/scsi/scsi_pass.h>

#include "drive_sensors.h"

#define MAX_DA_DEVS     10
#define LOG_SENSE_TEMP  0x0D
#define LOG_BUF_LEN     64
#define CAM_TIMEOUT_MS  5000

/* Send LOG SENSE page 0x0D (Temperature) via CAMIOCOMMAND on an open fd.
   Returns temperature in Celsius, or negative on any failure. */
static int
read_scsi_temp(int fd)
{
    uint8_t buf[LOG_BUF_LEN];
    memset(buf, 0, sizeof(buf));

    union ccb ccb;
    memset(&ccb, 0, sizeof(ccb));

    ccb.ccb_h.func_code = XPT_SCSI_IO;
    ccb.ccb_h.flags     = CAM_DIR_IN;
    ccb.ccb_h.timeout   = CAM_TIMEOUT_MS;

    ccb.csio.data_ptr   = buf;
    ccb.csio.dxfer_len  = LOG_BUF_LEN;
    ccb.csio.sense_len  = 32;
    ccb.csio.cdb_len    = 10;
    ccb.csio.tag_action = CAM_TAG_ACTION_NONE;

    /* LOG SENSE CDB (10-byte) */
    uint8_t *cdb = ccb.csio.cdb_io.cdb_bytes;
    cdb[0] = LOG_SENSE;                                  /* opcode 0x4D    */
    cdb[1] = 0;                                          /* SP=0           */
    cdb[2] = SLS_PAGE_CTRL_CUMULATIVE | LOG_SENSE_TEMP;  /* page 0x0D      */
    cdb[3] = 0;                                          /* subpage 0      */
    cdb[4] = 0;                                          /* reserved       */
    cdb[5] = 0;                                          /* param ptr hi   */
    cdb[6] = 0;                                          /* param ptr lo   */
    cdb[7] = 0;                                          /* alloc len hi   */
    cdb[8] = LOG_BUF_LEN;                               /* alloc len lo   */
    cdb[9] = 0;                                          /* control        */

    if (ioctl(fd, CAMIOCOMMAND, &ccb) < 0)
        return -1;
    if ((ccb.ccb_h.status & CAM_STATUS_MASK) != CAM_REQ_CMP)
        return -2;

    /* Temperature log page response:
     *   [0] page code 0x0D
     *   [2-3] page length
     *   [4-5] param code 0x0000
     *   [6] flags
     *   [7] param length = 2
     *   [8] reserved
     *   [9] temperature in Celsius (0xFF = not available)
     */
    if (buf[0] != LOG_SENSE_TEMP)
        return -3;
    if (buf[9] == 0xFF)
        return -4;  /* drive reports temp not available */

    return (int)(uint8_t)buf[9];
}

/* Find filesystem usage for a block device by scanning the live mount
   table. USB / extended-storage drives mount at varied points, so we
   match the device node rather than guessing daN → /mnt/extN.
   Returns 0 and fills total/used/free + mountpoint on success. */
static int
fs_usage_for_device(const char *devpath, uint64_t *total,
                    uint64_t *used, uint64_t *freeb,
                    char *mnt_out, size_t mnt_sz)
{
    struct statfs *mnt = NULL;
    int n = getmntinfo(&mnt, MNT_NOWAIT);
    if (n <= 0) return -1;
    size_t dl = strlen(devpath);
    for (int k = 0; k < n; k++) {
        const char *from = mnt[k].f_mntfromname;
        if (strncmp(from, devpath, dl) != 0) continue;
        /* next char must end the device name or start a partition
         * (not a digit, so /dev/da1 doesn't match /dev/da10) */
        char c = from[dl];
        if (c != '\0' && (c >= '0' && c <= '9')) continue;
        uint64_t bs = (uint64_t)mnt[k].f_bsize;
        uint64_t t  = (uint64_t)mnt[k].f_blocks * bs;
        uint64_t f  = (uint64_t)mnt[k].f_bfree  * bs;
        if (t == 0) continue;
        *total = t; *freeb = f; *used = (t > f) ? (t - f) : 0;
        if (mnt_out && mnt_sz) snprintf(mnt_out, mnt_sz, "%s", mnt[k].f_mntonname);
        return 0;
    }
    return -1;
}

/* Append a JSON object for one drive to the output buffer.
   Returns the new buffer position, or the original position if nothing
   was appended (not enough room).
   NOTE: snprintf returns the number of chars that *would have been
   written*. We clamp after each call so pos never exceeds cap, and
   we stop appending once truncation begins. */
static size_t
append_drive_json(char *buf, size_t cap, size_t pos, int first,
                  const char *devpath, off_t media_size,
                  const char *ident, int temp, int temp_err,
                  int has_fs, uint64_t fs_total, uint64_t fs_used,
                  uint64_t fs_free, const char *mount_point)
{
    /* Worst case: all fields populated ≈ 350 chars. Check once. */
    if (pos + 400 > cap) return pos;

    if (!first && pos + 1 < cap) {
        buf[pos++] = ',';
    }

    int n;
    n = snprintf(buf + pos, cap - pos,
        "{\"device\":\"%s\",\"sizeBytes\":%lld",
        devpath, (long long)media_size);
    if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
    pos += (size_t)n;

    if (ident && ident[0]) {
        n = snprintf(buf + pos, cap - pos, ",\"ident\":\"%s\"", ident);
        if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
        pos += (size_t)n;
    }

    if (temp >= 0) {
        n = snprintf(buf + pos, cap - pos, ",\"tempC\":%d", temp);
        if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
        pos += (size_t)n;
    } else if (temp_err != 0) {
        n = snprintf(buf + pos, cap - pos, ",\"tempErr\":%d", temp_err);
        if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
        pos += (size_t)n;
    }

    if (has_fs) {
        n = snprintf(buf + pos, cap - pos,
            ",\"fsTotalBytes\":%llu,\"fsUsedBytes\":%llu,\"fsFreeBytes\":%llu",
            (unsigned long long)fs_total,
            (unsigned long long)fs_used,
            (unsigned long long)fs_free);
        if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
        pos += (size_t)n;

        if (mount_point && mount_point[0]) {
            n = snprintf(buf + pos, cap - pos,
                ",\"mountPoint\":\"%s\"", mount_point);
            if ((size_t)n >= cap - pos) { pos = cap - 1; goto close_obj; }
            pos += (size_t)n;
        }
    }

close_obj:
    if (pos < cap) buf[pos++] = '}';
    return pos;
}

/* Append a JSON object for one fixed-storage entry. */
static size_t
append_fixed_json(char *buf, size_t cap, size_t pos, int first,
                  const char *label, uint64_t total,
                  uint64_t used, uint64_t freeb)
{
    if (pos + 200 > cap) return pos;
    if (!first && pos + 1 < cap) buf[pos++] = ',';
    int n = snprintf(buf + pos, cap - pos,
        "{\"label\":\"%s\",\"fsTotalBytes\":%llu,\"fsUsedBytes\":%llu,\"fsFreeBytes\":%llu}",
        label,
        (unsigned long long)total,
        (unsigned long long)used,
        (unsigned long long)freeb);
    if ((size_t)n >= cap - pos) return cap - 1;
    pos += (size_t)n;
    return pos;
}

int
drive_sensors_get_json(char *out, size_t out_cap, size_t *out_written,
                       const char **err_reason_out)
{
    if (!out || out_cap < 32) {
        if (err_reason_out) *err_reason_out = "buffer_too_small";
        return -1;
    }

    size_t pos = 0;
    pos += snprintf(out + pos, out_cap - pos, "{\"drives\":[");
    int drive_count = 0;

    for (int i = 0; i < MAX_DA_DEVS; i++) {
        char devpath[32];
        snprintf(devpath, sizeof(devpath), "/dev/da%d", i);

        int fd = open(devpath, O_RDONLY | O_NONBLOCK);
        if (fd < 0) {
            if (errno != ENOENT) {
                /* Device node exists but access denied — report it. */
                if (drive_count > 0 && pos < out_cap) out[pos++] = ',';
                int dn = snprintf(out + pos, out_cap - pos,
                    "{\"device\":\"%s\",\"accessDenied\":true}", devpath);
                if ((size_t)dn >= out_cap - pos) { pos = out_cap - 1; break; }
                pos += (size_t)dn;
                drive_count++;
            }
            continue;
        }

        /* Confirm it's a real block device. */
        off_t media_size = 0;
        if (ioctl(fd, DIOCGMEDIASIZE, &media_size) < 0) {
            close(fd);
            continue;
        }

        /* Disk identifier string (vendor/model/serial). */
        char ident[DISK_IDENT_SIZE];
        memset(ident, 0, sizeof(ident));
        const char *ident_str = NULL;
        if (ioctl(fd, DIOCGIDENT, ident) == 0 && ident[0] != '\0')
            ident_str = ident;

        /* Temperature via SCSI LOG SENSE. */
        int temp = read_scsi_temp(fd);
        close(fd);

        /* Filesystem usage from the mount table. */
        uint64_t fs_total = 0, fs_used = 0, fs_free = 0;
        char mount_point[128] = {0};
        int has_fs = (fs_usage_for_device(devpath, &fs_total, &fs_used,
                                          &fs_free, mount_point,
                                          sizeof(mount_point)) == 0);

        size_t new_pos = append_drive_json(out, out_cap, pos,
            drive_count == 0, devpath, media_size, ident_str,
            temp, (temp < 0) ? temp : 0, has_fs,
            fs_total, fs_used, fs_free, mount_point);
        if (new_pos > pos) {
            pos = new_pos;
            drive_count++;
        }
    }

    /* Close drives array, start storage array. */
    pos += snprintf(out + pos, out_cap - pos, "],\"storage\":[");

    /* Fixed PS5 storage: internal SSD + M.2 expansion. */
    static const struct { const char *label; const char *path; } fixed[] = {
        { "Internal SSD", "/user" },
        { "M.2 Expansion", "/mnt/ext1" },
    };

    int storage_count = 0;
    for (int j = 0; j < (int)(sizeof(fixed)/sizeof(fixed[0])); j++) {
        struct statvfs sv;
        if (statvfs(fixed[j].path, &sv) != 0 || sv.f_blocks == 0) continue;
        uint64_t total = (uint64_t)sv.f_blocks * sv.f_frsize;
        uint64_t freeb = (uint64_t)sv.f_bfree  * sv.f_frsize;
        uint64_t used  = (total > freeb) ? (total - freeb) : 0;
        pos = append_fixed_json(out, out_cap, pos, storage_count == 0,
                                fixed[j].label, total, used, freeb);
        storage_count++;
    }

    if (pos + 4 > out_cap) {
        if (err_reason_out) *err_reason_out = "buffer_overflow";
        return -1;
    }
    out[pos++] = ']';
    out[pos++] = '}';
    out[pos] = '\0';

    *out_written = pos;
    return 0;
}
