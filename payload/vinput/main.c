/* ps5u-vinput: the console's virtual keyboard and mouse, driven for the helper (payload/vinput/
 * README.md; protocol include/vinput_wire.h). The helper embeds this ELF and sends it to the
 * console's own loader (:9021) when a keyboard & mouse session opens; it is never sent on its own.
 * Single-threaded; listens on 127.0.0.1:VWIRE_PORT only. Its log: VWIRE_LOG_PATH. */
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include "vinput_companion.h"
#include "vinput_dev.h"
#include "vinput_wire.h"

static void vlog(const char *fmt, ...) {
    char line[384];
    struct stat st;
    va_list ap;
    int k = snprintf(line, sizeof line, "%lld [%d] ", (long long)time(NULL), (int)getpid());
    FILE *f;
    va_start(ap, fmt);
    vsnprintf(line + k, sizeof line - (size_t)k, fmt, ap);
    va_end(ap);
    if (stat(VWIRE_LOG_PATH, &st) == 0 && st.st_size > 64 * 1024) rename(VWIRE_LOG_PATH, VWIRE_LOG_PATH ".old");
    f = fopen(VWIRE_LOG_PATH, "a");
    if (!f) return;
    fprintf(f, "%s\n", line);
    fclose(f);
}

static void dev_init(void) {
    vinput_dev_init(vlog);
}

int main(void) {
    vcomp_cfg_t cfg = {0};
    int fd, code;
#ifdef SYS_thr_set_name
    (void)syscall(SYS_thr_set_name, -1, VWIRE_NAME);
#endif
    signal(SIGPIPE, SIG_IGN);
    /* elfldr hands its connection over as stdio: let it go. */
    fd = open("/dev/null", O_RDWR);
    if (fd >= 0) {
        dup2(fd, STDIN_FILENO);
        dup2(fd, STDOUT_FILENO);
        dup2(fd, STDERR_FILENO);
        if (fd > STDERR_FILENO) close(fd);
    }
    mkdir(VWIRE_DIR, 0777);
    vlog("start");
    cfg.dev = vinput_dev_backend();
    cfg.dev_init = dev_init;
    cfg.dev_tick = vinput_dev_tick;
    cfg.log = vlog;
    code = vcomp_run(&cfg);
    vlog("exit %d", code);
    return code;
}
