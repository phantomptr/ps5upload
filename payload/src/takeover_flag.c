/* Flag-file takeover between AVA1-era instances: see include/takeover_flag.h. */
#include "takeover_flag.h"
#include "proc_identity.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h> /* flock */
#include <sys/socket.h>
#include <sys/stat.h>
#if defined(__APPLE__) || defined(__FreeBSD__)
#include <sys/sysctl.h> /* KERN_ARND; glibc >= 2.32 has no such header and the use below is #ifdef KERN_ARND (review 007: Linux host build) */
#endif
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define FLAG_NAME "takeover"
#define FLAG_TMP "takeover.tmp"

static int flag_path(char *out, size_t cap, const char *dir, const char *name) {
    int n = snprintf(out, cap, "%s/%s", dir, name);
    return (n < 0 || (size_t)n >= cap) ? -1 : 0;
}

int takeover_flag_write(const char *dir, uint64_t nonce) {
    char tmp[300], dst[300], body[32];
    if (!dir || flag_path(tmp, sizeof tmp, dir, FLAG_TMP) || flag_path(dst, sizeof dst, dir, FLAG_NAME))
        return -1;
    int n = snprintf(body, sizeof body, "%llu\n", (unsigned long long)nonce);
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    int ok = write(fd, body, (size_t)n) == n;
    ok = (close(fd) == 0) && ok;
    /* Same directory, so never a cross-mount rename. */
    if (!ok || rename(tmp, dst) != 0) {
        (void)unlink(tmp);
        return -1;
    }
    return 0;
}

int takeover_flag_read(const char *dir, uint64_t *nonce) {
    char path[300], body[32];
    if (!dir || !nonce || flag_path(path, sizeof path, dir, FLAG_NAME)) return -1;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, body, sizeof body - 1);
    close(fd);
    if (n <= 0) return -1;
    body[n] = '\0';
    char *end = NULL;
    errno = 0;
    unsigned long long v = strtoull(body, &end, 10);
    if (errno != 0 || end == body || v == 0) return -1;
    *nonce = (uint64_t)v;
    return 0;
}

void takeover_flag_unlink(const char *dir) {
    char path[300];
    if (dir && flag_path(path, sizeof path, dir, FLAG_NAME) == 0) (void)unlink(path);
}

void takeover_flag_identity(const char *dir, takeover_flag_id_t *out) {
    char path[300];
    struct stat st;
    memset(out, 0, sizeof *out);
    if (!dir || flag_path(path, sizeof path, dir, FLAG_NAME) || stat(path, &st) != 0) return;
    if (takeover_flag_read(dir, &out->nonce) != 0) return;
    out->present = 1;
    out->ino = (uint64_t)st.st_ino;
#ifdef __APPLE__
    out->mtime_ns = (int64_t)st.st_mtimespec.tv_sec * 1000000000LL + st.st_mtimespec.tv_nsec;
#else
    out->mtime_ns = (int64_t)st.st_mtim.tv_sec * 1000000000LL + st.st_mtim.tv_nsec;
#endif
}

int takeover_flag_asks_us_to_exit(const char *dir, uint64_t my_nonce, const takeover_flag_id_t *stale) {
    takeover_flag_id_t now;
    takeover_flag_identity(dir, &now);
    if (!now.present || now.nonce == my_nonce) return 0;
    if (stale && stale->present && stale->nonce == now.nonce && stale->ino == now.ino &&
        stale->mtime_ns == now.mtime_ns)
        return 0;
    return 1;
}

int takeover_nonce_new(uint64_t *nonce) {
    uint64_t v = 0;
    int ok = 0;
#ifdef KERN_ARND
    {
        int mib[2] = {CTL_KERN, KERN_ARND};
        size_t len = sizeof v;
        ok = sysctl(mib, 2, &v, &len, NULL, 0) == 0 && len == sizeof v;
    }
#endif
    if (!ok) {
        int fd = open("/dev/urandom", O_RDONLY);
        if (fd >= 0) {
            ok = read(fd, &v, sizeof v) == (ssize_t)sizeof v;
            close(fd);
        }
    }
    if (!ok) {
        /* Last resort: mix the monotonic clock and the pid. Only distinctness matters here. */
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        v = ((uint64_t)ts.tv_nsec << 20) ^ (uint64_t)ts.tv_sec ^ ((uint64_t)getpid() << 40);
    }
    if (v == 0) v = 1;
    *nonce = v;
    return 0;
}

/* Fail closed: only a refused connect proves nobody listens. A socket we could not open (fd
 * pressure), an interrupted or failed connect, or anything else counts as "still answering", so
 * a new instance never starts its AVA1 side while the old one may hold the port (seen as a flaky
 * host test under load, ctest lifecycle.rs).
 *
 * BOUNDED: the connect is non-blocking and waited on for TAKEOVER_PROBE_MS. A blocking connect to
 * a listener that has stopped accepting (a wedged predecessor whose backlog is full: the kernel
 * drops the SYN and retries for over a minute) parked the caller for that minute PER PROBE, and
 * the callers count probes, not time. No answer in time is "still answering": something holds the
 * port. */
#define TAKEOVER_PROBE_MS 500
int takeover_port_responding(int port) {
    struct sockaddr_in a;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return 1;
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) != 0) {
        close(fd);
        return 1; /* cannot bound the probe: do not make it, and do not call the port free */
    }
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int rc = connect(fd, (struct sockaddr *)&a, sizeof a);
    int err = rc == 0 ? 0 : errno;
    if (rc != 0 && err == EINPROGRESS) {
        struct pollfd pfd;
        pfd.fd = fd;
        pfd.events = POLLOUT;
        pfd.revents = 0;
        int pr = poll(&pfd, 1, TAKEOVER_PROBE_MS);
        if (pr > 0) {
            int soerr = 0;
            socklen_t sl = sizeof soerr;
            if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0) soerr = errno ? errno : EIO;
            err = soerr;
            rc = soerr == 0 ? 0 : -1;
        } else {
            err = pr == 0 ? ETIMEDOUT : (errno ? errno : EIO); /* neither is ECONNREFUSED: busy */
        }
    }
    close(fd);
    if (rc != 0 && err != ECONNREFUSED)
        fprintf(stderr, "[takeover] port %d probe: connect failed errno=%d, treating as busy\n", port, err);
    return rc == 0 || err != ECONNREFUSED;
}

int takeover_wait_port_free(int port, int max_ms, int interval_ms) {
    struct timespec t0, now;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    if (interval_ms < 1) interval_ms = 1;
    for (;;) {
        long waited;
        if (!takeover_port_responding(port)) return 0;
        clock_gettime(CLOCK_MONOTONIC, &now);
        /* Whole nanoseconds first: dividing a negative tv_nsec difference rounds toward zero,
         * which reported up to 1 ms more than had passed and ended the wait early. */
        waited = (long)((((long long)(now.tv_sec - t0.tv_sec)) * 1000000000LL + (now.tv_nsec - t0.tv_nsec)) / 1000000LL);
        if (waited >= max_ms) return -1;
        usleep((useconds_t)interval_ms * 1000u);
    }
}

int takeover_gate_start(int port, int max_ms, int interval_ms, void (*start)(void *), void *ctx) {
    if (takeover_wait_port_free(port, max_ms, interval_ms) != 0) return 0;
    start(ctx);
    return 1;
}

static long long mono_ms_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000L;
}

int takeover_flag_request(const char *dir, uint64_t my_nonce, const int *ports, int nports,
                          int attempts, int interval_us) {
    if (takeover_flag_write(dir, my_nonce) != 0) return -1;
    /* Bounded by the monotonic clock, not by the number of probes: a probe of a wedged predecessor
     * takes its whole TAKEOVER_PROBE_MS, which turned a 10 s wait into about a minute. */
    if (attempts < 1) attempts = 1;
    if (interval_us < 0) interval_us = 0;
    long long budget_ms = ((long long)attempts * interval_us) / 1000LL;
    long long t0 = mono_ms_now();
    for (;;) {
        int busy = 0;
        for (int p = 0; p < nports; p++) busy |= takeover_port_responding(ports[p]);
        if (!busy) {
            takeover_flag_unlink(dir);
            return 0;
        }
        if (mono_ms_now() - t0 >= budget_ms) return -1;
        usleep((useconds_t)interval_us);
    }
}

#define START_LOCK_NAME "start.lock"

/* The host self-test builds with -DTAKEOVER_FLOCK=<its own> to stand in for a file system or
 * firmware where flock fails. */
#ifndef TAKEOVER_FLOCK
#define TAKEOVER_FLOCK flock
#else
int TAKEOVER_FLOCK(int fd, int op);
#endif

/* One try. Fills *outcome (TAKEOVER_START_HELD / _NO_FILE / _NO_FLOCK / _STUCK, or 0 for busy),
 * *err (the errno behind _NO_FILE / _NO_FLOCK) and *age_ms (the holder's age, -1 when unknown). */
static int start_lock_try(const char *dir, int max_age_ms, int *holder_pid, int *outcome, int *err,
                          long long *age_ms) {
    char path[300], body[64];
    *holder_pid = 0;
    *outcome = 0;
    *err = 0;
    *age_ms = -1;
    if (!dir || flag_path(path, sizeof path, dir, START_LOCK_NAME)) {
        *outcome = TAKEOVER_START_NO_FILE;
        return TAKEOVER_START_NO_LOCK;
    }
    /* Never O_TRUNC and never unlinked: the file is only a name for the kernel's lock, and the lock
     * dies with its holder, so a file left by a crash or a power cut holds nothing. */
    int fd = open(path, O_RDWR | O_CREAT, 0644);
    if (fd < 0) {
        *err = errno;
        *outcome = TAKEOVER_START_NO_FILE;
        return TAKEOVER_START_NO_LOCK;
    }
    if (TAKEOVER_FLOCK(fd, LOCK_EX | LOCK_NB) == 0) {
        /* Best effort: if this fails the lock is held all the same, only the holder's age is unknown
         * to the others (they then bound their wait by their own clock). */
        int n = snprintf(body, sizeof body, "%d %lld\n", (int)getpid(), mono_ms_now());
        if (ftruncate(fd, 0) == 0 && n > 0) (void)pwrite(fd, body, (size_t)n, 0);
        *outcome = TAKEOVER_START_HELD;
        return fd;
    }
    int e = errno;
    if (e != EWOULDBLOCK && e != EAGAIN) {
        /* No locks on this file system or firmware (EOPNOTSUPP, EINVAL, ENOLCK, ...): start as before,
         * without one. Never read as "held". */
        close(fd);
        *err = e;
        *outcome = TAKEOVER_START_NO_FLOCK;
        return TAKEOVER_START_NO_LOCK;
    }
    /* Another helper is starting. For how long? */
    int pid = 0;
    long long since = -1;
    ssize_t got = pread(fd, body, sizeof body - 1, 0);
    close(fd);
    if (got > 0) {
        body[got] = '\0';
        if (sscanf(body, "%d %lld", &pid, &since) != 2 || since < 0) {
            pid = 0;
            since = -1;
        }
    }
    *holder_pid = pid;
    /* Unreadable: the holder took the lock this instant and has not written yet (or could not write).
     * A stamp ahead of our clock is a body left from an earlier boot that the holder could not
     * overwrite. Either way the age is unknown. */
    if (since < 0) return TAKEOVER_START_BUSY;
    long long age = mono_ms_now() - since;
    if (age < 0) return TAKEOVER_START_BUSY;
    *age_ms = age;
    if (age > (long long)max_age_ms) {
        *outcome = TAKEOVER_START_STUCK;
        return TAKEOVER_START_NO_LOCK;
    }
    return TAKEOVER_START_BUSY;
}

int takeover_start_lock(const char *dir, int max_age_ms, int *holder_pid) {
    int pid = 0, outcome = 0, err = 0;
    long long age = -1;
    int r = start_lock_try(dir, max_age_ms, &pid, &outcome, &err, &age);
    if (holder_pid) *holder_pid = pid;
    return r;
}

int takeover_start_lock_wait(const char *dir, int max_age_ms, int poll_ms, takeover_start_info_t *info) {
    takeover_start_info_t local;
    takeover_start_info_t *in = info ? info : &local;
    memset(in, 0, sizeof *in);
    in->holder_age_ms = -1;
    if (poll_ms < 1) poll_ms = 1;
    long long t0 = mono_ms_now();
    long long since_holder = t0; /* when the holder we are waiting on was first seen */
    int last_pid = -1;
    for (;;) {
        int pid = 0, outcome = 0, err = 0;
        long long age = -1;
        int r = start_lock_try(dir, max_age_ms, &pid, &outcome, &err, &age);
        long long now = mono_ms_now();
        in->waited_ms = now - t0;
        if (r != TAKEOVER_START_BUSY) {
            in->outcome = outcome;
            in->flock_errno = err;
            if (outcome == TAKEOVER_START_STUCK) {
                in->holder_pid = pid;
                in->holder_age_ms = age;
            }
            return r;
        }
        in->waited = 1;
        in->holder_pid = pid;
        in->holder_age_ms = age;
        if (pid != last_pid) {
            /* A different holder (the one before finished and a third start took the lock): its
             * start is judged from now, not from when this one began waiting. */
            last_pid = pid;
            since_holder = now;
        }
        /* The holder's own stamp is unknown or unusable: bound the wait by our own clock, so a holder
         * that hangs without a readable age cannot keep this start waiting for good. */
        if (age < 0 && now - since_holder > (long long)max_age_ms) {
            in->outcome = TAKEOVER_START_STUCK;
            return TAKEOVER_START_NO_LOCK;
        }
        usleep((useconds_t)poll_ms * 1000u);
    }
}

void takeover_start_unlock(int fd) {
    if (fd >= 0) close(fd); /* closing the only descriptor releases the lock */
}

typedef struct {
    char dir[240];
    uint64_t my_nonce;
    int period_ms;
    void (*on_newer)(void);
    takeover_flag_id_t stale;
} poll_ctx_t;

static void *poll_thread(void *arg) {
    proc_name_set_self(PS5UPLOAD2_PROC_NAME); /* unnamed, the process can list as payload.elf */
    poll_ctx_t c = *(poll_ctx_t *)arg;
    free(arg);
    for (;;) {
        struct timespec ts = {c.period_ms / 1000, (long)(c.period_ms % 1000) * 1000000L};
        nanosleep(&ts, NULL);
        if (takeover_flag_asks_us_to_exit(c.dir, c.my_nonce, &c.stale)) {
            c.on_newer();
            return NULL;
        }
    }
}

int takeover_flag_poll_start(const char *dir, uint64_t my_nonce, int period_ms, void (*on_newer)(void)) {
    if (!dir || !on_newer || period_ms <= 0 || strlen(dir) >= sizeof(((poll_ctx_t *)0)->dir)) return -1;
    poll_ctx_t *c = calloc(1, sizeof *c);
    if (!c) return -1;
    memcpy(c->dir, dir, strlen(dir) + 1);
    c->my_nonce = my_nonce;
    c->period_ms = period_ms;
    c->on_newer = on_newer;
    /* Whatever is there now is from before this instance: remember it as stale and remove it. */
    takeover_flag_identity(dir, &c->stale);
    takeover_flag_unlink(dir);
    pthread_t t;
    pthread_attr_t a;
    if (pthread_attr_init(&a) != 0) {
        free(c);
        return -1;
    }
    (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&t, &a, poll_thread, c);
    pthread_attr_destroy(&a);
    if (rc != 0) {
        free(c);
        return -1;
    }
    return 0;
}
