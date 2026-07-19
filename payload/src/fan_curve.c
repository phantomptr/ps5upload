#include "fan_curve.h"

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <pthread.h>

#include "config.h"
#include "hw_info.h"

#define FAN_CURVE_DIR  PS5UPLOAD2_RUNTIME_ROOT
#define FAN_CURVE_FILE PS5UPLOAD2_RUNTIME_ROOT "/fan_curve.json"

static pthread_mutex_t g_fan_curve_mtx = PTHREAD_MUTEX_INITIALIZER;

static int ensure_dir(const char *path) {
    struct stat st;
    if (stat(path, &st) == 0) return 0;
    char tmp[256];
    size_t len = strnlen(path, sizeof(tmp));
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            mkdir(tmp, 0755);
            *p = '/';
        }
    }
    return mkdir(tmp, 0755);
}

/* Extract the first "temp_c" integer value from a JSON fan-curve body.
 *
 * The engine sends: {"points":[{"temp_c":65,"duty_pct":40},...]}.
 * We scan for the first "\"temp_c\":NN" occurrence and parse NN. This
 * is a minimal scanner matching the extract_json_* style used in
 * runtime.c — no full JSON parser is linked into the payload.
 *
 * Returns the temperature in °C, or -1 if not found. */
static int fan_curve_parse_first_temp(const char *json) {
    if (!json) return -1;
    static const char needle[] = "\"temp_c\":";
    const char *pos = strstr(json, needle);
    if (!pos) return -1;
    pos += sizeof(needle) - 1;
    /* Skip optional whitespace between : and the number. */
    while (*pos == ' ' || *pos == '\t') pos++;
    if (*pos < '0' || *pos > '9') return -1;
    return (int)strtol(pos, NULL, 10);
}

int fan_curve_set(const char *points_json, char *err, size_t err_cap) {
    if (!points_json || !points_json[0]) {
        if (err && err_cap > 0) snprintf(err, err_cap, "missing points");
        return -1;
    }

    pthread_mutex_lock(&g_fan_curve_mtx);

    ensure_dir(FAN_CURVE_DIR);
    int fd = open(FAN_CURVE_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        if (err && err_cap > 0)
            snprintf(err, err_cap, "cannot open %s", FAN_CURVE_FILE);
        pthread_mutex_unlock(&g_fan_curve_mtx);
        return -1;
    }
    size_t len = strlen(points_json);
    ssize_t w = write(fd, points_json, len);
    close(fd);
    if (w < 0 || (size_t)w != len) {
        if (err && err_cap > 0)
            snprintf(err, err_cap, "write failed");
        pthread_mutex_unlock(&g_fan_curve_mtx);
        return -1;
    }

    /* Apply to hardware via hw_info's canonical setter rather than a
     * private ioctl. hw_fan_set_threshold does the /dev/icc_fan write AND
     * pins + persists the value so the 15 s auto-reapply watcher keeps it
     * alive across the fan-state resets the firmware issues on every game
     * launch. A one-shot ioctl here would silently revert on the next
     * launch — the whole reason hw_info owns the watcher. The PS5's fan
     * ioctl only accepts a single threshold, so a multi-point curve maps
     * to its first (ramp-start) point, which is the knob users care about.
     * Non-fatal: the curve is already persisted, so a hardware failure
     * still lets the user inspect/retry from the UI. */
    int threshold = fan_curve_parse_first_temp(points_json);
    if (threshold >= 0) {
        const char *reason = NULL;
        (void)hw_fan_set_threshold((uint8_t)threshold, &reason);
    }

    pthread_mutex_unlock(&g_fan_curve_mtx);
    return 0;
}

int fan_curve_get(char *buf, size_t cap) {
    if (!buf || cap == 0) return -1;

    pthread_mutex_lock(&g_fan_curve_mtx);

    int fd = open(FAN_CURVE_FILE, O_RDONLY);
    if (fd < 0) {
        /* No saved curve — return an empty-points JSON so the caller
         * can distinguish "no config" from a parse error. */
        int len = snprintf(buf, cap, "{\"points\":[]}");
        pthread_mutex_unlock(&g_fan_curve_mtx);
        return len > 0 ? 0 : -1;
    }

    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) {
        pthread_mutex_unlock(&g_fan_curve_mtx);
        return -1;
    }
    /* If the file is larger than our buffer, the read was truncated.
     * Persisting this truncated version on the next fan_curve_set would
     * silently destroy curve data. Return an error instead. */
    if (n == (ssize_t)(cap - 1)) {
        struct stat st;
        if (stat(FAN_CURVE_FILE, &st) == 0 && (size_t)st.st_size > cap - 1) {
            pthread_mutex_unlock(&g_fan_curve_mtx);
            return -1;
        }
    }
    buf[n] = '\0';

    pthread_mutex_unlock(&g_fan_curve_mtx);
    return 0;
}
