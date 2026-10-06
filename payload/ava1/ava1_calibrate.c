/* disk.calibrate: file creation and fsync cost at 1/2/4/8/16 workers. Every point
 * creates a private directory and removes it before answering. */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_gen.h"
#include "ava1_manifest.h"
#include "ava1_thread.h"

#define CAL_POINTS 5
#define CAL_MAX_FILES 20000u
#define CAL_MAX_SIZE (1u << 20)
#define CAL_PATH (AVA1_MAX_PATH + 64)

static const uint8_t WORKERS[CAL_POINTS] = { 1, 2, 4, 8, 16 };

typedef struct {
    char dir[CAL_PATH];
    uint32_t first, count, size;
    int *fds;
    const uint8_t *data;
    uint64_t elapsed;
    int sync_phase, err;
    const char *step; /* what failed, when err != 0 */
} cal_worker_t;

static uint32_t g_cal_open; /* fds held by calibrate workers right now (test peak below) */

static void cal_open_add(int d) {
    uint32_t now = (uint32_t)(__atomic_add_fetch(&g_cal_open, (uint32_t)d, __ATOMIC_SEQ_CST));
    if (d > 0) {
        uint32_t pk = __atomic_load_n(&ava1_data_test_cal_peak, __ATOMIC_SEQ_CST);
        while (now > pk &&
               !__atomic_compare_exchange_n(&ava1_data_test_cal_peak, &pk, now, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {}
    }
}

static void *cal_worker(void *arg) {
    cal_worker_t *c = arg;
    uint64_t start = ava1_mono_us();
    uint32_t i;
    for (i = 0; i < c->count; i++) {
        char path[CAL_PATH];
        int fd;
        if (!c->sync_phase) {
            if (snprintf(path, sizeof path, "%s/%u", c->dir, c->first + i) >= (int)sizeof path) {
                c->err = ENAMETOOLONG;
                c->step = "name";
                break;
            }
            fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0644);
            if (fd < 0) { c->err = errno; c->step = "create"; break; }
            c->fds[i] = fd;
            cal_open_add(1);
            if (c->size && write(fd, c->data, c->size) != (ssize_t)c->size) {
                c->err = errno ? errno : EIO;
                c->step = "write";
                break;
            }
            if (fchmod(fd, 0644) != 0) { c->err = errno; c->step = "chmod"; break; }
        } else {
            fd = c->fds[i];
            if (fsync(fd) != 0) { c->err = errno; c->step = "fsync"; break; }
            c->fds[i] = -1;
            cal_open_add(-1);
            if (close(fd) != 0) { c->err = errno; c->step = "close"; break; }
        }
    }
    c->elapsed = ava1_mono_us() - start;
    return NULL;
}

static void cleanup(const char *sub, uint32_t files, int *fds, uint32_t nfds) {
    char path[CAL_PATH];
    uint32_t i;
    for (i = 0; i < nfds; i++)
        if (fds[i] >= 0) { close(fds[i]); fds[i] = -1; cal_open_add(-1); }
    for (i = 0; i < files; i++) {
        if (snprintf(path, sizeof path, "%s/%u", sub, i) < (int)sizeof path) (void)unlink(path);
    }
    (void)rmdir(sub);
}

static uint32_t saturate_u32(uint64_t n) { return n > UINT32_MAX ? UINT32_MAX : (uint32_t)n; }

int ava1_calibrate(const uint8_t *body, uint32_t len, uint8_t *out, size_t cap, size_t *out_len) {
    ava1_disk_calibrate_t q;
    ava1_disk_calibrate_result_t result;
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    char dir[AVA1_MAX_PATH + 1];
    uint8_t blob[CAL_POINTS * 40];
    ava1_w_t points, encoded;
    uint8_t *data = NULL;
    int *fds = NULL;
    uint32_t chunk = 0;
    char why[1536] = "";
    int status = AVA1_STATUS_OK;
    unsigned k;
    *out_len = 0;
    memset(&q, 0, sizeof q);
    if (ava1_disk_calibrate_decode(body, len, &q) != 0 || !q.dir_len || q.dir_len > AVA1_MAX_PATH ||
        !q.files || q.files > CAL_MAX_FILES || q.size > CAL_MAX_SIZE) {
        ava1_rpc_msg(out, cap, out_len, "disk.calibrate: files must be 1..%u and size at most %u bytes", CAL_MAX_FILES,
                     CAL_MAX_SIZE);
        return AVA1_ERR_PROTOCOL;
    }
    memcpy(dir, q.dir, q.dir_len);
    dir[q.dir_len] = 0;
    if (dir[0] != '/' || !ava1_path_ok((const uint8_t *)dir + 1, q.dir_len - 1)) {
        ava1_rpc_msg(out, cap, out_len, "disk.calibrate: not an absolute, valid path: %s", dir);
        return AVA1_ERR_PATH;
    }
    if (!cfg->may_write || !cfg->may_write(dir)) {
        ava1_rpc_msg(out, cap, out_len, "disk.calibrate: %s is not writable by AVA1 policy", dir);
        return AVA1_ERR_PATH;
    }
    data = malloc(q.size ? q.size : 1);
    /* Never more than the open-file budget's share at once: a chunk is created, written and
     * fsynced, then closed, before the next (the measured create/fsync costs per file are the same). */
    chunk = ava1_pend_share();
    if (chunk < 16) chunk = 16;
    if (chunk > q.files) chunk = q.files;
    fds = malloc((size_t)chunk * sizeof *fds);
    if (!data || !fds) { status = AVA1_ERR_INTERNAL; goto done; }
    memset(data, 0x5a, q.size);
    ava1_w_init(&points, blob, sizeof blob);
    for (k = 0; k < CAL_POINTS; k++) {
        char sub[CAL_PATH];
        cal_worker_t c[16];
        pthread_t threads[16];
        uint64_t wall_start, wall, create_total = 0, sync_total = 0;
        unsigned phase, t;
        uint32_t base;
        ava1_cal_point_t pt;
        if (snprintf(sub, sizeof sub, "%s/.ava-cal-%u", dir, WORKERS[k]) >= (int)sizeof sub) {
            status = AVA1_ERR_PATH;
            snprintf(why, sizeof why, "disk.calibrate: path too long");
            break;
        }
        /* An existing directory is not ours to overwrite or sweep. */
        if (mkdir(sub, 0777) != 0) {
            status = errno == EEXIST ? AVA1_ERR_EXISTS : AVA1_ERR_IO;
            snprintf(why, sizeof why, "disk.calibrate: mkdir %s failed: %s", sub, strerror(errno));
            break;
        }
        for (t = 0; t < chunk; t++) fds[t] = -1;
        wall_start = ava1_mono_us();
        for (base = 0; base < q.files && status == AVA1_STATUS_OK; base += chunk) {
            uint32_t n = q.files - base < chunk ? q.files - base : chunk;
            uint32_t per = (n + WORKERS[k] - 1) / WORKERS[k];
            for (phase = 0; phase < 2 && status == AVA1_STATUS_OK; phase++) {
                unsigned started = 0;
                for (t = 0; t < WORKERS[k]; t++) {
                    uint32_t first = t * per;
                    uint32_t left = first >= n ? 0 : n - first;
                    memset(&c[t], 0, sizeof c[t]);
                    snprintf(c[t].dir, sizeof c[t].dir, "%s", sub);
                    c[t].first = base + first;
                    c[t].count = left < per ? left : per;
                    c[t].size = q.size;
                    c[t].fds = fds + (first < n ? first : n);
                    c[t].data = data;
                    c[t].sync_phase = (int)phase;
                    if (ava1_thread_start(cal_worker, &c[t], &threads[t]) != 0) {
                        status = AVA1_ERR_INTERNAL;
                        snprintf(why, sizeof why, "disk.calibrate: cannot start a worker thread (workers=%u)", WORKERS[k]);
                        break;
                    }
                    started++;
                }
                for (t = 0; t < started; t++) pthread_join(threads[t], NULL);
                for (t = 0; t < started; t++) {
                    if (c[t].err && status == AVA1_STATUS_OK) {
                        status = AVA1_ERR_IO;
                        snprintf(why, sizeof why, "disk.calibrate: %s failed at %u workers, file %u of %u: %s",
                                 c[t].step ? c[t].step : "io", WORKERS[k], c[t].first, q.files,
                                 strerror(c[t].err));
                    }
                    if (phase == 0) create_total += c[t].elapsed;
                    else sync_total += c[t].elapsed;
                }
            }
        }
        wall = ava1_mono_us() - wall_start;
        cleanup(sub, q.files, fds, chunk);
        if (status != AVA1_STATUS_OK) break;
        memset(&pt, 0, sizeof pt);
        pt.workers = WORKERS[k];
        pt.files_per_s = saturate_u32(wall ? (uint64_t)q.files * 1000000u / wall : UINT32_MAX);
        pt.create_us = saturate_u32(create_total / q.files);
        pt.fsync_us = saturate_u32(sync_total / q.files);
        if (ava1_cal_point_append(&points, &pt) != 0) { status = AVA1_ERR_INTERNAL; break; }
    }
    if (status == AVA1_STATUS_OK) {
        memset(&result, 0, sizeof result);
        result.points = blob;
        result.points_len = (uint32_t)points.len;
        ava1_w_init(&encoded, out, cap);
        if (ava1_disk_calibrate_result_encode(&result, &encoded) == 0) *out_len = encoded.len;
        else status = AVA1_ERR_INTERNAL;
    }
done:
    if (status != AVA1_STATUS_OK)
        fprintf(stderr, "[ava1] disk.calibrate failed (status %d, files=%u size=%u chunk=%u): %s\n", status, q.files,
                q.size, chunk, why[0] ? why : "no detail");
    if (status != AVA1_STATUS_OK && why[0]) ava1_rpc_msg(out, cap, out_len, "%s", why);
    else if (status == AVA1_ERR_INTERNAL && !why[0]) ava1_rpc_msg(out, cap, out_len, "disk.calibrate: out of memory");
    free(data);
    free(fds);
    return status;
}
