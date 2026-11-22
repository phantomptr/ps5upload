/* Host self-test for the start/stop trace (start_trace.h, #903): one line per call with the pid
 * and version, written to disk at once; control characters cannot split a line; the file moves to
 * .old past its size cap; the fallback path is used when the folder is missing; errno survives a
 * call; an unmount is bracketed by BEGIN/END lines. Built with -DSTART_TRACE_MAX=300. */
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "config.h"
#include "start_trace.h"

static char g_dir[256], g_log[300], g_old[300], g_fb[300];

static size_t slurp(const char *path, char *buf, size_t cap) {
    FILE *f = fopen(path, "rb");
    size_t n;
    if (!f) return 0;
    n = fread(buf, 1, cap - 1, f);
    fclose(f);
    buf[n] = '\0';
    return n;
}

static int count_lines(const char *s) {
    int n = 0;
    for (; *s; s++)
        if (*s == '\n') n++;
    return n;
}

static void test_format(void) {
    char b[128];
    int n = start_trace_format(b, sizeof b, 1763740756LL, 7, 42, "12.6.19", "TAKEOVER_DONE");
    assert(n > 0);
    assert(strcmp(b, "1763740756.007 pid=42 v12.6.19 TAKEOVER_DONE\n") == 0);
    /* A newline or a control character in the stage cannot make two lines. */
    n = start_trace_format(b, sizeof b, 1, 0, 1, "v", "A\nB\rC\tD");
    assert(n > 0 && count_lines(b) == 1 && b[n - 1] == '\n');
    assert(strstr(b, "A?B?C?D\n"));
    /* Too small: refused, never a cut line without its newline. */
    assert(start_trace_format(b, 10, 1763740756LL, 7, 42, "12.6.19", "TAKEOVER_DONE") == -1);
}

static void test_write_and_errno(void) {
    char buf[4096], want[64];
    start_trace_set_paths(g_log, g_old, g_fb);
    errno = EBADF;
    start_trace("ENTER_MAIN");
    assert(errno == EBADF);
    start_tracef("REAP_WAIT pid=%d grace_ms=%d", 77, 15000);
    assert(errno == EBADF);
    assert(slurp(g_log, buf, sizeof buf) > 0);
    assert(count_lines(buf) == 2);
    snprintf(want, sizeof want, " pid=%d v%s ENTER_MAIN\n", (int)getpid(), PS5UPLOAD2_VERSION);
    assert(strstr(buf, want));
    assert(strstr(buf, " REAP_WAIT pid=77 grace_ms=15000\n"));
}

static void test_unmount_bracketed(void) {
    char buf[4096], path[320];
    int rc, e;
    snprintf(path, sizeof path, "%s/not-a-mount", g_dir);
    (void)unlink(g_log);
    rc = start_trace_unmount(path, 0, "selftest");
    e = errno;
    assert(rc != 0 && e != 0);
    assert(slurp(g_log, buf, sizeof buf) > 0);
    assert(count_lines(buf) == 2);
    assert(strstr(buf, "UNMOUNT_BEGIN ") && strstr(buf, "by=selftest"));
    {
        char end[400];
        snprintf(end, sizeof end, "UNMOUNT_END %s rc=-1 errno=%d\n", path, e);
        assert(strstr(buf, end));
    }
}

static void test_rotation(void) {
    char buf[4096];
    struct stat st;
    (void)unlink(g_log);
    (void)unlink(g_old);
    for (int i = 0; i < 20; i++) start_tracef("FILLER %d", i);
    assert(stat(g_old, &st) == 0 && st.st_size > 300);
    assert(stat(g_log, &st) == 0 && st.st_size <= 300 + 128);
    /* Every line is whole in one file or the other. */
    assert(slurp(g_log, buf, sizeof buf) > 0 && buf[strlen(buf) - 1] == '\n');
    assert(slurp(g_old, buf, sizeof buf) > 0 && buf[strlen(buf) - 1] == '\n');
}

static void test_fallback(void) {
    char buf[1024], missing[320];
    snprintf(missing, sizeof missing, "%s/no-such-dir/startup.log", g_dir);
    start_trace_set_paths(missing, g_old, g_fb);
    start_trace("BEFORE_ENSURE_DIRECTORIES");
    assert(slurp(g_fb, buf, sizeof buf) > 0 && strstr(buf, " BEFORE_ENSURE_DIRECTORIES\n"));
    start_trace_set_paths(NULL, NULL, NULL);
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "/tmp/ps5u-start-trace-%d", (int)getpid());
    assert(mkdir(g_dir, 0755) == 0);
    snprintf(g_log, sizeof g_log, "%s/startup.log", g_dir);
    snprintf(g_old, sizeof g_old, "%s/startup.log.old", g_dir);
    snprintf(g_fb, sizeof g_fb, "%s/fallback.log", g_dir);
    test_format();
    test_write_and_errno();
    test_unmount_bracketed();
    test_rotation();
    test_fallback();
    (void)unlink(g_log);
    (void)unlink(g_old);
    (void)unlink(g_fb);
    (void)rmdir(g_dir);
    puts("start_trace selftest: ok");
    return 0;
}
