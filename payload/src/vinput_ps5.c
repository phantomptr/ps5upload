/* The console's own virtual keyboard and mouse, as the helper sees them: the relay backend
 * (src/vinput_relay.c) to the companion process ps5u-vinput (payload/vinput/), which this file
 * launches through the console's own loader and ends by pid. The helper itself never calls
 * libSceMouse / libSceKeyboard: sceMouseInit / sceKeyboardInit hung the whole helper when it did
 * (payload/vinput/README.md).
 *
 * vinput_ps5_backend() hands the relay out when BOTH hold:
 *   - the firmware is on vinput_ps5_firmware_allowed's list (only "13.60": the one the calls were
 *     probed and tested end to end on), and
 *   - it is not switched off: built with -DVINPUT_PS5=0, or the file VINPUT_PS5_DISABLE_FILE
 *     exists (a way out for a console where the companion misbehaves, without a new helper).
 * Otherwise the helper keeps vinput.c's stub (no devices, "vinput_unsupported"). */
#include "vinput_ps5.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#include "ps5_firmware.h"
#include "vinput_relay.h"

#ifndef VINPUT_PS5
#define VINPUT_PS5 1
#endif

#define LOADER_PORT 9021
/* elfldr closes :9021 for about a second while the PC app swaps in its patched build (and for 3 s
 * after a failed accept): a refused port is tried again this long before "no loader". */
#define LOADER_RETRY_MS 4000

/* The companion ELF (stripped), embedded by the Makefile (build/vinput_elf.c). */
extern unsigned char vinput_elf[];
extern unsigned int vinput_elf_len;

static long long vps5_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/* A loopback connection to the loader: the fd, -2 refused, -1 otherwise. */
static int dial_loader(void) {
    struct sockaddr_in a;
    struct pollfd p;
    int fd = socket(AF_INET, SOCK_STREAM, 0), fl, rc, soerr = 0;
    socklen_t sl = sizeof soerr;
    if (fd < 0) return -1;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(LOADER_PORT);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    rc = connect(fd, (struct sockaddr *)&a, sizeof a);
    if (rc != 0 && errno != EINPROGRESS) {
        int e = errno;
        close(fd);
        return e == ECONNREFUSED ? -2 : -1;
    }
    if (rc != 0) {
        p.fd = fd;
        p.events = POLLOUT;
        p.revents = 0;
        if (poll(&p, 1, 2000) <= 0 || getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0 || soerr) {
            close(fd);
            return soerr == ECONNREFUSED ? -2 : -1;
        }
    }
    fcntl(fd, F_SETFL, fl);
    return fd;
}

/* The ELF to the loader: all of it, then a clean close (half-close, a bounded wait for the
 * loader's side, close). elfldr hands this connection to the payload as its stdio; the companion
 * points its stdio at /dev/null at once. */
static int vps5_launch(void *ctx) {
    long long until = vps5_now_ms() + LOADER_RETRY_MS;
    const unsigned char *b = vinput_elf;
    size_t n = vinput_elf_len;
    int fd;
    (void)ctx;
    for (;;) {
        fd = dial_loader();
        if (fd >= 0) break;
        if (vps5_now_ms() >= until) return fd == -2 ? VRELAY_NO_LOADER : -1;
        usleep(300 * 1000);
    }
    {
        struct timeval to = {5, 0};
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &to, sizeof to);
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);
    }
    while (n) {
        ssize_t w = send(fd, b, n, 0);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) {
            close(fd);
            return -1;
        }
        b += w;
        n -= (size_t)w;
    }
    shutdown(fd, SHUT_WR);
    {
        long long stop = vps5_now_ms() + 1000;
        char sink[256];
        while (vps5_now_ms() < stop) {
            struct pollfd p = {fd, POLLIN, 0};
            if (poll(&p, 1, 250) <= 0) continue;
            if (recv(fd, sink, sizeof sink, 0) <= 0) break;
        }
    }
    close(fd);
    fprintf(stderr, "[vinput] companion sent to the loader (%u bytes)\n", vinput_elf_len);
    return 0;
}

/* SIGTERM first: the companion stops its loop, lets go of what it held and deletes its devices (a
 * SIGKILLed one leaves them behind until the next one starts). SIGKILL if it is still there after
 * VPS5_TERM_WAIT_MS (stuck in a call). Runs on the relay's thread without its lock. */
#define VPS5_TERM_WAIT_MS 300

static void vps5_kill(void *ctx, int pid) {
    long long until;
    (void)ctx;
    if (pid <= 1 || pid == getpid()) return;
    fprintf(stderr, "[vinput] SIGTERM companion pid %d: %d\n", pid, kill(pid, SIGTERM));
    until = vps5_now_ms() + VPS5_TERM_WAIT_MS;
    while (vps5_now_ms() < until) {
        if (kill(pid, 0) != 0 && errno == ESRCH) return;
        usleep(20 * 1000);
    }
    fprintf(stderr, "[vinput] SIGKILL companion pid %d: %d\n", pid, kill(pid, SIGKILL));
}

const vinput_backend_t *vinput_ps5_backend(void) {
    char kv[256], fw[32];
    size_t len = sizeof kv;
    struct stat st;
    vrelay_cfg_t cfg;
    int on = VINPUT_PS5 && stat(VINPUT_PS5_DISABLE_FILE, &st) != 0;
    if (sysctlbyname("kern.version", kv, &len, NULL, 0) != 0) kv[0] = '\0';
    kv[sizeof kv - 1] = '\0';
    ps5_firmware_from_kernel(kv, fw, sizeof fw);
    if (!on || !vinput_ps5_firmware_allowed(fw)) {
        fprintf(stderr, "[vinput] console devices %s (firmware %s): the stub answers\n", on ? "not allowed" : "off",
                fw);
        return NULL;
    }
    memset(&cfg, 0, sizeof cfg);
    cfg.launch = vps5_launch;
    cfg.kill = vps5_kill;
    vinput_relay_configure(&cfg);
    fprintf(stderr, "[vinput] console devices on (firmware %s): relayed to %s on :%d\n", fw, "ps5u-vinput", 9184);
    return vinput_relay_backend();
}
