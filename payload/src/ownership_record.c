#include "ownership_record.h"

#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int ownership_record_format(char *buf, size_t n, uint64_t instance_id, int port, int startup_reason,
                            uint64_t started, int pid) {
    int w;
    if (!buf || started == 0 || pid <= 0) return -1;
    w = snprintf(buf, n, "instance_id=%llu\nruntime_port=%d\nstartup_reason=%d\nstarted_at_unix=%llu\npid=%d\n",
                 (unsigned long long)instance_id, port, startup_reason, (unsigned long long)started, pid);
    if (w < 0 || (size_t)w >= n) return -1;
    return w;
}

int ownership_record_parse(const char *text, size_t len, ownership_rec_t *r) {
    size_t i = 0;
    memset(r, 0, sizeof *r);
    while (i < len) {
        const char *nl = memchr(text + i, '\n', len - i);
        char line[128];
        size_t ll;
        unsigned long long v;
        int pid;
        if (!nl) break; /* an unterminated last line may be cut short: not evidence */
        ll = (size_t)(nl - (text + i));
        if (ll < sizeof line) {
            memcpy(line, text + i, ll);
            line[ll] = 0;
            if (sscanf(line, "started_at_unix=%llu", &v) == 1) r->started = (uint64_t)v;
            else if (sscanf(line, "instance_id=%llu", &v) == 1) r->instance_id = (uint64_t)v;
            else if (sscanf(line, "pid=%d", &pid) == 1 && pid > 0) r->pid = pid;
        }
        i = (size_t)(nl - text) + 1;
    }
    return r->pid > 0 && r->started != 0;
}

static int read_once(const char *path, ownership_rec_t *r) {
    char buf[512];
    ssize_t got = 0, k;
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        memset(r, 0, sizeof *r);
        return 0;
    }
    while (got < (ssize_t)sizeof buf) {
        k = read(fd, buf + got, sizeof buf - (size_t)got);
        if (k <= 0) break;
        got += k;
    }
    close(fd);
    return ownership_record_parse(buf, (size_t)got, r);
}

int ownership_record_read(const char *path, ownership_rec_t *r, int retry_ms) {
    if (read_once(path, r)) return 1;
    if (retry_ms > 0) usleep((useconds_t)retry_ms * 1000u);
    return read_once(path, r);
}

void ownership_record_merge(ownership_rec_t *fresh, const ownership_rec_t *snap) {
    if (fresh->pid <= 0) {
        *fresh = *snap;
        return;
    }
    if (fresh->pid != snap->pid) return;
    if (fresh->started == 0) fresh->started = snap->started;
    if (fresh->instance_id == 0) fresh->instance_id = snap->instance_id;
}
