/* Start lock and the takeover wait (takeover_flag.c), host build.
 *
 * Exactly one of two helpers starting together holds the lock; the other waits and touches nothing,
 * then starts once the first serves, dies or is taken for stuck: somebody always ends up serving
 * (#877). A lock left by a process that died (crash, power cut) never stops the next start; flock
 * failing is never read as "held". The takeover wait ends on the monotonic clock, not after a count
 * of probes. Built with -DTAKEOVER_FLOCK=test_flock. fork() here is the host test only: the helper
 * itself never forks. */
#include "takeover_flag.h"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <errno.h>
#include <sys/file.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

static long long mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000L;
}

/* Stands in for flock (the build passes -DTAKEOVER_FLOCK=test_flock): a file system or firmware
 * where flock fails with g_flock_errno. Inherited by forked children. */
static int g_flock_errno = 0;
int test_flock(int fd, int op);
int test_flock(int fd, int op) {
    if (g_flock_errno) {
        errno = g_flock_errno;
        return -1;
    }
    return flock(fd, op);
}

/* ---- Simulated helpers: separate processes running main()'s start-up order ----
 * lock (wait) -> [behaviour] -> take over whoever serves (flag) -> bind -> flag poll -> unlock. */
enum { SIM_OK, SIM_SLOW, SIM_DIE_BEFORE_SERVE, SIM_HANG, SIM_HANG_NO_STAMP, SIM_OLD_NO_LOCK };
#define SIM_MAX_AGE_MS 1500

static void sim_on_newer(void) { _exit(0); } /* asked to leave by a newer helper: a clean exit */

static int port_answers(int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int ok = fd >= 0 && connect(fd, (struct sockaddr *)&a, sizeof a) == 0;
    if (fd >= 0) close(fd);
    return ok;
}

static pid_t sim_start(const char *dir, int port, int behaviour) {
    pid_t pid = fork();
    if (pid != 0) return pid;
    int lock = -1;
    if (behaviour != SIM_OLD_NO_LOCK) lock = takeover_start_lock_wait(dir, SIM_MAX_AGE_MS, 20, NULL);
    if (behaviour == SIM_HANG_NO_STAMP && lock >= 0 && ftruncate(lock, 0) != 0) _exit(9);
    if (behaviour == SIM_HANG || behaviour == SIM_HANG_NO_STAMP)
        for (;;) pause();
    if (behaviour == SIM_SLOW || behaviour == SIM_DIE_BEFORE_SERVE) usleep(700 * 1000);
    if (behaviour == SIM_DIE_BEFORE_SERVE) _exit(3);
    uint64_t nonce = 0;
    (void)takeover_nonce_new(&nonce);
    int ports[1] = {port};
    if (port_answers(port) && takeover_flag_request(dir, nonce, ports, 1, 100, 20000) != 0) _exit(4);
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    (void)setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (s < 0 || bind(s, (struct sockaddr *)&a, sizeof a) != 0 || listen(s, 16) != 0) _exit(5);
    if (takeover_flag_poll_start(dir, nonce, 50, sim_on_newer) != 0) _exit(6);
    takeover_start_unlock(lock);
    for (;;) pause();
}

/* 1 = exited with `code` within ms; 0 = still running (or another status). */
static int exited_with(pid_t p, int code, int ms) {
    for (int i = 0; i < ms / 10; i++) {
        int st = 0;
        pid_t r = waitpid(p, &st, WNOHANG);
        if (r == p) return WIFEXITED(st) && WEXITSTATUS(st) == code;
        usleep(10 * 1000);
    }
    return 0;
}

static int alive(pid_t p) { return waitpid(p, NULL, WNOHANG) == 0; }

static int port_served_within(int port, int ms) {
    for (int i = 0; i < ms / 20; i++) {
        if (port_answers(port)) return 1;
        usleep(20 * 1000);
    }
    return 0;
}

static void reap(pid_t p) {
    if (p <= 0) return;
    kill(p, SIGKILL);
    waitpid(p, NULL, 0);
}

static int free_port(void) {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    socklen_t sl = sizeof sa;
    int port = -1;
    if (s >= 0 && bind(s, (struct sockaddr *)&sa, sizeof sa) == 0 && getsockname(s, (struct sockaddr *)&sa, &sl) == 0)
        port = (int)ntohs(sa.sin_port);
    if (s >= 0) close(s);
    return port;
}

/* Two (or three) helpers sent within moments of each other, and the ways the first can go wrong.
 * The rule under test: somebody always ends up serving, and the start-ups never overlap. */
static void simulated_starts(const char *dir) {
    int port = free_port();
    check(port > 0, "sim: a free loopback port");

    /* 1. Both fine, the first slow to serve: the second waits, then takes over (the one sent last runs). */
    {
        pid_t a = sim_start(dir, port, SIM_SLOW);
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_OK);
        check(!exited_with(b, 0, 400), "sim slow winner: the second start waits, it does not leave");
        check(exited_with(a, 0, 5000), "sim slow winner: the first serves, then hands over to the second");
        check(alive(b) && port_served_within(port, 3000), "sim slow winner: the second serves");
        reap(a);
        reap(b);
    }
    /* 2. The first dies before it serves: the one that waited starts instead (12.6.13: nobody served). */
    {
        pid_t a = sim_start(dir, port, SIM_DIE_BEFORE_SERVE);
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_OK);
        check(exited_with(a, 3, 3000), "sim winner dies: the first dies before serving");
        check(alive(b) && port_served_within(port, 3000), "sim winner dies: the second serves");
        reap(a);
        reap(b);
    }
    /* 3. The first hangs, its stamp readable: past max_age the second starts without the lock. */
    {
        pid_t a = sim_start(dir, port, SIM_HANG);
        usleep(50 * 1000);
        long long t0 = mono_ms();
        pid_t b = sim_start(dir, port, SIM_OK);
        int served = port_served_within(port, SIM_MAX_AGE_MS + 3000);
        long long took = mono_ms() - t0;
        check(served && alive(b), "sim winner hangs: the second serves");
        check(took >= SIM_MAX_AGE_MS - 200, "sim winner hangs: only once the first is past max_age");
        reap(a);
        reap(b);
    }
    /* 4. The first hangs and its age cannot be read: the second's own wait is the bound. */
    {
        pid_t a = sim_start(dir, port, SIM_HANG_NO_STAMP);
        usleep(100 * 1000);
        pid_t b = sim_start(dir, port, SIM_OK);
        check(port_served_within(port, SIM_MAX_AGE_MS + 3000) && alive(b),
              "sim winner hangs with no readable age: the second serves after its own max_age");
        reap(a);
        reap(b);
    }
    /* 5. An older helper (no start lock) already serving: the new one takes the lock at once and takes over. */
    {
        pid_t old = sim_start(dir, port, SIM_OLD_NO_LOCK);
        check(port_served_within(port, 2000), "sim older helper: serving without the lock");
        pid_t b = sim_start(dir, port, SIM_OK);
        check(exited_with(old, 0, 3000), "sim older helper: asked to leave by the new one");
        check(alive(b) && port_served_within(port, 2000), "sim older helper: the new one serves");
        reap(old);
        reap(b);
    }
    /* 6. Three at once: one after another; which of the two waiters goes next is open, but exactly
     * one helper is left, serving. */
    {
        pid_t a = sim_start(dir, port, SIM_SLOW);
        usleep(30 * 1000);
        pid_t b = sim_start(dir, port, SIM_SLOW);
        usleep(30 * 1000);
        pid_t c = sim_start(dir, port, SIM_SLOW);
        check(exited_with(a, 0, 6000), "sim three starts: the first hands over");
        int b_gone = 0, c_gone = 0;
        for (int i = 0; i < 800 && !b_gone && !c_gone; i++) {
            int st = 0;
            if (waitpid(b, &st, WNOHANG) == b) b_gone = WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 1 : -1;
            if (waitpid(c, &st, WNOHANG) == c) c_gone = WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 1 : -1;
            usleep(10 * 1000);
        }
        check((b_gone == 1 && alive(c)) || (c_gone == 1 && alive(b)),
              "sim three starts: one waiter hands over to the other");
        check(port_served_within(port, 3000), "sim three starts: the last one serves");
        reap(a);
        reap(b);
        reap(c);
    }
    /* 7. No flock on this file system: both start as before the lock existed, and one serves. */
    {
        g_flock_errno = EOPNOTSUPP;
        pid_t a = sim_start(dir, port, SIM_SLOW);
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_OK);
        int served = port_served_within(port, 3000);
        usleep(1500 * 1000);
        check(served && port_answers(port) && (alive(a) || alive(b)),
              "sim no flock: a helper still serves at the end");
        reap(a);
        reap(b);
        g_flock_errno = 0;
    }
}

static void write_lock_body(const char *dir, const char *body) {
    char path[300];
    snprintf(path, sizeof path, "%s/start.lock", dir);
    int fd = open(path, O_RDWR | O_CREAT, 0644);
    if (fd < 0) return;
    if (ftruncate(fd, 0) == 0) (void)pwrite(fd, body, strlen(body), 0);
    close(fd);
}

int main(void) {
    char dir[] = "/tmp/ps5u-takeover-lock-XXXXXX";
    if (!mkdtemp(dir)) {
        perror("mkdtemp");
        return 1;
    }

    /* One holder at a time. */
    int pid = -1;
    int a = takeover_start_lock(dir, 120000, &pid);
    check(a >= 0, "the first start holds the lock");
    check(pid == 0, "the holder is told no other pid");
    int b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_BUSY, "a second start while the first holds it is told BUSY");
    check(pid == (int)getpid(), "and is told the holder's pid");
    takeover_start_unlock(a);
    int c = takeover_start_lock(dir, 120000, &pid);
    check(c >= 0, "once released, the next start holds it");
    takeover_start_unlock(c);

    /* A file left behind holds nothing: whatever it says, nobody holds the kernel lock. */
    write_lock_body(dir, "4242 1\n");
    c = takeover_start_lock(dir, 120000, &pid);
    check(c >= 0, "a file left by a crash or power cut does not stop the start");
    takeover_start_unlock(c);

    /* A holder that dies: the kernel drops its lock. */
    {
        int gate[2];
        if (pipe(gate) != 0) return 1;
        pid_t child = fork();
        if (child == 0) {
            int fd = takeover_start_lock(dir, 120000, NULL);
            char ok = fd >= 0 ? 'y' : 'n';
            (void)write(gate[1], &ok, 1);
            for (;;) pause();
        }
        char got = 0;
        (void)read(gate[0], &got, 1);
        check(got == 'y', "a child process takes the lock");
        int busy = takeover_start_lock(dir, 120000, &pid);
        check(busy == TAKEOVER_START_BUSY && pid == (int)child, "while it lives, the parent is told BUSY");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        c = takeover_start_lock(dir, 120000, &pid);
        check(c >= 0, "once it is killed, the next start holds the lock");
        takeover_start_unlock(c);
        close(gate[0]);
        close(gate[1]);
    }

    /* A holder that has been starting for longer than any start takes is taken for stuck. */
    a = takeover_start_lock(dir, 120000, NULL);
    {
        char body[64];
        snprintf(body, sizeof body, "%d %lld\n", (int)getpid(), mono_ms() - 200000);
        write_lock_body(dir, body);
    }
    b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_NO_LOCK, "a holder starting for over max_age: start without the lock");
    /* Taken this instant, not written yet: busy, never "stuck". */
    write_lock_body(dir, "");
    b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_BUSY, "a holder that has not written its age yet: BUSY");
    takeover_start_unlock(a);

    check(takeover_start_lock("/nonexistent-dir-for-takeover-test", 120000, &pid) == TAKEOVER_START_NO_LOCK,
          "no lock file can be made: start without the lock");

    /* The waiting gate. */
    {
        takeover_start_info_t in;
        int fd = takeover_start_lock_wait(dir, 120000, 10, &in);
        check(fd >= 0 && in.outcome == TAKEOVER_START_HELD && !in.waited, "wait: a free lock is held at once");
        takeover_start_unlock(fd);

        fd = takeover_start_lock_wait("/nonexistent-dir-for-takeover-test", 120000, 10, &in);
        check(fd == TAKEOVER_START_NO_LOCK && in.outcome == TAKEOVER_START_NO_FILE,
              "wait: no lock file: start without the lock, not BUSY");

        /* flock failing other than "held" is never read as held, whatever the errno. */
        int errs[3] = {EOPNOTSUPP, EINVAL, ENOLCK};
        for (int i = 0; i < 3; i++) {
            g_flock_errno = errs[i];
            long long t0 = mono_ms();
            fd = takeover_start_lock_wait(dir, 120000, 10, &in);
            check(fd == TAKEOVER_START_NO_LOCK && in.outcome == TAKEOVER_START_NO_FLOCK &&
                      in.flock_errno == errs[i] && mono_ms() - t0 < 500,
                  "wait: flock fails (EOPNOTSUPP / EINVAL / ENOLCK): start at once without the lock");
            check(takeover_start_lock(dir, 120000, &pid) == TAKEOVER_START_NO_LOCK,
                  "try: flock fails: NO_LOCK, never BUSY");
        }
        g_flock_errno = 0;

        /* A holder whose stamp is from an earlier boot (ahead of this clock) and never rewritten:
         * the wait is bounded by our own clock. */
        a = takeover_start_lock(dir, 120000, NULL);
        {
            char body[64];
            snprintf(body, sizeof body, "%d %lld\n", 4242, mono_ms() + 10000000LL);
            write_lock_body(dir, body);
        }
        long long t0 = mono_ms();
        fd = takeover_start_lock_wait(dir, 400, 10, &in);
        long long took = mono_ms() - t0;
        check(fd == TAKEOVER_START_NO_LOCK && in.outcome == TAKEOVER_START_STUCK && in.waited,
              "wait: a holder with a stamp from another boot: taken for stuck");
        check(took >= 350 && took < 3000, "wait: after this start's own max_age");
        takeover_start_unlock(a);
    }

    simulated_starts(dir);

    /* The takeover wait ends by the monotonic clock. A listener that never goes away. */
    {
        int s = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in sa;
        memset(&sa, 0, sizeof sa);
        sa.sin_family = AF_INET;
        sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        sa.sin_port = 0;
        socklen_t sl = sizeof sa;
        int ok = s >= 0 && bind(s, (struct sockaddr *)&sa, sizeof sa) == 0 && listen(s, 64) == 0 &&
                 getsockname(s, (struct sockaddr *)&sa, &sl) == 0;
        check(ok, "a loopback listener stands in for the old helper");
        int ports[1] = {(int)ntohs(sa.sin_port)};
        long long t0 = mono_ms();
        /* 5 x 100 ms = 500 ms budget. */
        int rc = takeover_flag_request(dir, 7, ports, 1, 5, 100000);
        long long took = mono_ms() - t0;
        check(rc == -1, "a port that stays busy: the wait gives up");
        check(took >= 450 && took < 2000, "within the budget on the monotonic clock");
        close(s);
        rc = takeover_flag_request(dir, 7, ports, 1, 5, 100000);
        check(rc == 0, "a port that is free: the wait ends at once");
        takeover_flag_unlink(dir);
    }

    char path[300];
    snprintf(path, sizeof path, "%s/start.lock", dir);
    unlink(path);
    takeover_flag_unlink(dir);
    snprintf(path, sizeof path, "%s/takeover.tmp", dir);
    unlink(path);
    rmdir(dir);
    if (failures) {
        printf("takeover lock self-test: %d failure(s)\n", failures);
        return 1;
    }
    printf("takeover lock self-test: all passed\n");
    return 0;
}
