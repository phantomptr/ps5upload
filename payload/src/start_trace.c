/* The start/stop trace (start_trace.h). */
#include "start_trace.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/param.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "config.h"

#define START_TRACE_PATH PS5UPLOAD2_RUNTIME_ROOT "/startup.log"
#define START_TRACE_OLD PS5UPLOAD2_RUNTIME_ROOT "/startup.log.old"
#define START_TRACE_FALLBACK "/data/ps5upload_startup.log"

static const char *g_path = START_TRACE_PATH;
static const char *g_old = START_TRACE_OLD;
static const char *g_fallback = START_TRACE_FALLBACK;

void start_trace_set_paths(const char *path, const char *old_path, const char *fallback) {
    g_path = path ? path : START_TRACE_PATH;
    g_old = old_path ? old_path : START_TRACE_OLD;
    g_fallback = fallback ? fallback : START_TRACE_FALLBACK;
}

int start_trace_format(char *buf, size_t cap, long long sec, long ms, int pid, const char *version,
                       const char *stage) {
    int n = snprintf(buf, cap, "%lld.%03ld pid=%d v%s %s\n", sec, ms, pid, version ? version : "?",
                     stage ? stage : "");
    if (n < 0 || (size_t)n >= cap) return -1;
    /* Everything between the prefix and the newline is the stage: no control character may split
     * it into two lines or hide the newline. */
    for (int i = 0; i < n - 1; i++) {
        unsigned char c = (unsigned char)buf[i];
        if (c < 0x20 || c == 0x7f) buf[i] = '?';
    }
    return n;
}

static int open_append(const char *path) { return open(path, O_WRONLY | O_CREAT | O_APPEND, 0644); }

static void write_line(const char *line, size_t len) {
    int fd = open_append(g_path);
    if (fd < 0) {
        fd = open_append(g_fallback);
        if (fd < 0) return;
    } else {
        struct stat st;
        if (fstat(fd, &st) == 0 && st.st_size > (off_t)START_TRACE_MAX) {
            /* Siblings in one folder: never across a mount (cross_device.h). Two helpers rotating
             * at the same moment lose one generation at worst, never a line being written. */
            close(fd);
            (void)rename(g_path, g_old);
            fd = open_append(g_path);
            if (fd < 0) return;
        }
    }
    ssize_t w;
    do {
        w = write(fd, line, len);
    } while (w < 0 && errno == EINTR);
    (void)fsync(fd);
    close(fd);
}

void start_trace(const char *stage) {
    char line[512];
    struct timespec ts;
    int saved = errno; /* callers log around system calls whose errno they still read */
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        ts.tv_sec = time(NULL);
        ts.tv_nsec = 0;
    }
    int n = start_trace_format(line, sizeof line, (long long)ts.tv_sec, (long)(ts.tv_nsec / 1000000),
                               (int)getpid(), PS5UPLOAD2_VERSION, stage);
    if (n > 0) write_line(line, (size_t)n);
    errno = saved;
}

int start_trace_unmount(const char *path, int flags, const char *why) {
    int rc, e;
    start_tracef("UNMOUNT_BEGIN %s flags=0x%x by=%s", path ? path : "(null)", (unsigned)flags, why ? why : "?");
    rc = unmount(path, flags);
    e = errno;
    start_tracef("UNMOUNT_END %s rc=%d errno=%d", path ? path : "(null)", rc, rc != 0 ? e : 0);
    errno = e;
    return rc;
}

void start_tracef(const char *fmt, ...) {
    char stage[401];
    va_list ap;
    int saved = errno;
    va_start(ap, fmt);
    (void)vsnprintf(stage, sizeof stage, fmt, ap);
    va_end(ap);
    errno = saved;
    start_trace(stage);
}
