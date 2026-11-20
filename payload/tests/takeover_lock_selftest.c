/* Start lock and the takeover wait (takeover_flag.c), host build.
 *
 * Exactly one of two helpers starting together holds the lock; the other waits and touches nothing,
 * then starts once the first serves, dies or is taken for stuck: somebody always ends up serving
 * (#877). A lock left by a process that died (crash, power cut) never stops the next start; flock
 * failing is never read as "held". The takeover wait ends on the monotonic clock, not after a count
 * of probes. Built with -DTAKEOVER_FLOCK=test_flock. fork() here is the host test only: the helper
 * itself never forks. */
#include "takeover_flag.h"
#include "start_dedupe.h"
#include "helper_busy.h"

#include <pthread.h>
#include <stdint.h>

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
enum { SIM_OK, SIM_SLOW, SIM_DIE_BEFORE_SERVE, SIM_HANG, SIM_HANG_NO_STAMP, SIM_OLD_NO_LOCK, SIM_DEDUPE };
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

/* The version the next simulated helper says it is, and whether its transfer port stays mute (it
 * accepts and never answers a frame). Set before sim_start; the forked child keeps its copy. */
static const char *g_sim_version = "12.6.15";
static int g_sim_mute = 0;

/* A stand-in AVA1 server: reads a 16-byte frame header and answers with an Error frame header
 * ("A1", type 0x0b, valid CRC32C), as the real server answers an Hs1 that does not decode. */
static uint32_t test_crc32c(const uint8_t *p, size_t n) {
    uint32_t c = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; i++) {
        c ^= p[i];
        for (int k = 0; k < 8; k++) c = (c >> 1) ^ (0x82F63B78u & (0u - (c & 1u)));
    }
    return c ^ 0xFFFFFFFFu;
}

static void *sim_responder(void *arg) {
    int s = (int)(intptr_t)arg;
    for (;;) {
        int c = accept(s, NULL, NULL);
        if (c < 0) continue;
        uint8_t in[16], out[16];
        size_t have = 0;
        while (have < sizeof in) {
            ssize_t n = read(c, in + have, sizeof in - have);
            if (n <= 0) break;
            have += (size_t)n;
        }
        if (have == sizeof in && !g_sim_mute) {
            memset(out, 0, sizeof out);
            out[0] = 'A';
            out[1] = '1';
            out[2] = 0x0b;
            uint32_t crc = test_crc32c(out, 12);
            out[12] = (uint8_t)crc;
            out[13] = (uint8_t)(crc >> 8);
            out[14] = (uint8_t)(crc >> 16);
            out[15] = (uint8_t)(crc >> 24);
            (void)write(c, out, sizeof out);
        }
        if (g_sim_mute) {
            usleep(3000 * 1000);
        }
        close(c);
    }
    return NULL;
}

static pid_t sim_start(const char *dir, int port, int behaviour) {
    pid_t pid = fork();
    if (pid != 0) return pid;
    int lock = -1;
    takeover_start_info_t info;
    memset(&info, 0, sizeof info);
    if (behaviour != SIM_OLD_NO_LOCK) lock = takeover_start_lock_wait(dir, SIM_MAX_AGE_MS, 20, &info);
    /* main.c's dedupe: a helper that waited leaves the start to a recent, same-or-newer helper
     * that really serves. */
    if (behaviour == SIM_DEDUPE && info.waited && lock >= 0) {
        serving_rec_t rec;
        int ok = serving_record_read(dir, &rec);
        int pid_alive = ok && kill(rec.pid, 0) == 0;
        if (start_dedupe_should_stand_down(&rec, ok, g_sim_version, start_dedupe_mono_ms(), (int)getpid(),
                                           pid_alive) &&
            start_dedupe_probe_serves(port, 1000)) {
            takeover_start_unlock(lock);
            _exit(7);
        }
    }
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
    {
        pthread_t t;
        if (pthread_create(&t, NULL, sim_responder, (void *)(intptr_t)s) != 0) _exit(8);
        pthread_detach(t);
    }
    /* Serving: the record main.c writes before it lets the next start go. */
    {
        serving_rec_t me;
        memset(&me, 0, sizeof me);
        me.pid = (int)getpid();
        snprintf(me.version, sizeof me.version, "%s", g_sim_version);
        me.mono_ms = start_dedupe_mono_ms();
        (void)serving_record_write(dir, &me);
    }
    takeover_start_unlock(lock);
    for (;;) pause();
}

/* Two helpers sent together (12.6.15): the one that waited leaves the start to a helper of the same
 * or a newer version that began serving moments ago and answers; in every other case it takes
 * over, so a helper always ends up serving. */
static void simulated_dedupe(const char *dir, int port);

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

static void simulated_dedupe(const char *dir, int port) {
    char rec_path[300];
    snprintf(rec_path, sizeof rec_path, "%s/serving", dir);
    /* 1. Same version, sent together: the second leaves quietly, the first keeps serving. One
     * restart instead of two. */
    {
        unlink(rec_path);
        g_sim_version = "12.6.15";
        pid_t a = sim_start(dir, port, SIM_SLOW);
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_DEDUPE);
        check(exited_with(b, 7, 5000), "dedupe same version: the one that waited leaves the start to the first");
        check(alive(a) && port_served_within(port, 1000), "dedupe same version: the first keeps serving");
        reap(a);
        reap(b);
    }
    /* 2. The first is an older helper: the newer one that waited takes over, as before. */
    {
        unlink(rec_path);
        g_sim_version = "12.6.14";
        pid_t a = sim_start(dir, port, SIM_SLOW);
        usleep(50 * 1000);
        g_sim_version = "12.6.15";
        pid_t b = sim_start(dir, port, SIM_DEDUPE);
        check(exited_with(a, 0, 6000), "dedupe older first: the older one is asked to leave");
        check(alive(b) && port_served_within(port, 3000), "dedupe older first: the newer one serves");
        reap(a);
        reap(b);
    }
    /* 3. The first dies before it serves (no record of it): the one that waited serves. */
    {
        unlink(rec_path);
        g_sim_version = "12.6.15";
        pid_t a = sim_start(dir, port, SIM_DIE_BEFORE_SERVE);
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_DEDUPE);
        check(exited_with(a, 3, 3000), "dedupe first dies: the first dies before serving");
        check(alive(b) && port_served_within(port, 3000), "dedupe first dies: the one that waited serves");
        reap(a);
        reap(b);
    }
    /* 4. The first serves but does not answer a frame (wedged): the one that waited takes over. */
    {
        unlink(rec_path);
        g_sim_version = "12.6.15";
        g_sim_mute = 1;
        pid_t a = sim_start(dir, port, SIM_SLOW);
        g_sim_mute = 0;
        usleep(50 * 1000);
        pid_t b = sim_start(dir, port, SIM_DEDUPE);
        check(exited_with(a, 0, 8000), "dedupe mute first: it is asked to leave");
        check(alive(b) && port_served_within(port, 3000), "dedupe mute first: the one that waited serves");
        reap(a);
        reap(b);
    }
    unlink(rec_path);
}

/* The pieces of the dedupe and the busy report, one by one. */
static void dedupe_units(const char *dir) {
    /* Versions. */
    check(start_dedupe_version_cmp("12.6.15", "12.6.15") == 0, "version: equal");
    check(start_dedupe_version_cmp("12.6.14", "12.6.15") == -1, "version: older");
    check(start_dedupe_version_cmp("12.10.0", "12.9.9") == 1, "version: numbers, not text");
    check(start_dedupe_version_cmp("12.6.15-rc.1", "12.6.15") == -1, "version: a pre-release is below its release");
    check(start_dedupe_version_cmp("v12.6", "12.6.0") == 0, "version: 'v' and two parts");
    check(start_dedupe_version_cmp("garbage", "12.6.15") == -2, "version: not a version");
    check(start_dedupe_version_cmp("12.6.15x", "12.6.15") == -2, "version: trailing junk is not a version");

    /* The record. */
    serving_rec_t r, back;
    memset(&r, 0, sizeof r);
    r.pid = 4242;
    snprintf(r.version, sizeof r.version, "12.6.15");
    r.mono_ms = 123456;
    char buf[128];
    int n = serving_record_format(buf, sizeof buf, &r);
    check(n > 0 && serving_record_parse(buf, (size_t)n, &back) && back.pid == 4242 &&
              strcmp(back.version, "12.6.15") == 0 && back.mono_ms == 123456,
          "record: round trip");
    check(!serving_record_parse(buf, (size_t)n - 1, &back), "record: a read caught mid-write is incomplete");
    check(!serving_record_parse("pid=1\nversion=1.0.0\n", 20, &back), "record: a missing field is incomplete");
    r.pid = 0;
    check(serving_record_format(buf, sizeof buf, &r) == -1, "record: no pid is never written");
    r.pid = (int)getpid();
    check(serving_record_write(dir, &r) == 0 && serving_record_read(dir, &back) && back.pid == (int)getpid(),
          "record: written and read back");
    serving_record_clear(dir, 999999);
    check(serving_record_read(dir, &back), "record: another process's exit leaves it");
    serving_record_clear(dir, (int)getpid());
    check(!serving_record_read(dir, &back), "record: its own exit removes it");

    /* The decision. */
    serving_rec_t s;
    memset(&s, 0, sizeof s);
    s.pid = 77;
    snprintf(s.version, sizeof s.version, "12.6.15");
    s.mono_ms = 1000000;
    long long now = 1000000 + 5000;
    check(start_dedupe_should_stand_down(&s, 1, "12.6.15", now, 1, 1), "decide: same version, just started: stand down");
    check(start_dedupe_should_stand_down(&s, 1, "12.6.14", now, 1, 1), "decide: newer one serving: stand down");
    check(!start_dedupe_should_stand_down(&s, 1, "12.7.0", now, 1, 1), "decide: older one serving: take over");
    check(!start_dedupe_should_stand_down(&s, 1, "12.6.15", 1000000 + START_DEDUPE_RECENT_MS + 1, 1, 1),
          "decide: serving for long: take over (someone asked for a new one)");
    check(!start_dedupe_should_stand_down(&s, 1, "12.6.15", now, 1, 0), "decide: its process is gone: take over");
    check(!start_dedupe_should_stand_down(&s, 0, "12.6.15", now, 1, 1), "decide: no complete record: take over");
    check(!start_dedupe_should_stand_down(&s, 1, "12.6.15", now, 77, 1), "decide: our own record: take over");
    check(!start_dedupe_should_stand_down(&s, 1, "12.6.15", 999000, 1, 1), "decide: a record from the future: take over");
    snprintf(s.version, sizeof s.version, "dev");
    check(!start_dedupe_should_stand_down(&s, 1, "12.6.15", now, 1, 1), "decide: a version that does not read: take over");

    /* The loopback round trip. */
    {
        int port = free_port();
        check(!start_dedupe_probe_serves(port, 300), "probe: nothing listening: no");
        int ls = socket(AF_INET, SOCK_STREAM, 0), one = 1;
        struct sockaddr_in a;
        memset(&a, 0, sizeof a);
        a.sin_family = AF_INET;
        a.sin_port = htons((uint16_t)port);
        a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        (void)setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
        int ok = ls >= 0 && bind(ls, (struct sockaddr *)&a, sizeof a) == 0 && listen(ls, 4) == 0;
        check(ok, "probe: a stand-in server");
        pthread_t t;
        g_sim_mute = 0;
        pthread_create(&t, NULL, sim_responder, (void *)(intptr_t)ls);
        pthread_detach(t);
        check(start_dedupe_probe_serves(port, 1000), "probe: a server that answers a frame: yes");
        g_sim_mute = 1;
        long long t0 = mono_ms();
        check(!start_dedupe_probe_serves(port, 400), "probe: a server that takes the connection and never answers: no");
        check(mono_ms() - t0 < 1500, "probe: bounded by its timeout");
        g_sim_mute = 0;
    }

    /* The busy report (helper_busy.h). */
    helper_busy_t b;
    memset(&b, 0, sizeof b);
    char js[256];
    check(helper_busy_cause(&b) == NULL, "busy: nothing running: idle");
    b.other_devices = 3;
    check(helper_busy_cause(&b) == NULL, "busy: other devices only connected: still idle");
    check(helper_busy_json(&b, js, sizeof js) > 0 && strstr(js, "\"idle\":true") && strstr(js, "\"other_devices\":3"),
          "busy: json says idle");
    b.extract = 1;
    b.uploads = 2;
    check(helper_busy_cause(&b) && strcmp(helper_busy_cause(&b), "uploads") == 0, "busy: the first cause is named");
    check(helper_busy_json(&b, js, sizeof js) > 0 && strstr(js, "\"idle\":false") && strstr(js, "\"uploads\":2"),
          "busy: json says busy");
    check(helper_busy_json(&b, js, 10) == -1, "busy: a buffer too small is refused, not cut");
    memset(&b, 0, sizeof b);
    b.pads = 1;
    check(helper_busy_cause(&b) && strcmp(helper_busy_cause(&b), "pads") == 0, "busy: virtual controllers count");
    check(helper_shutdown_if_idle("{\"if_idle\":true}", 16), "if_idle: true");
    check(helper_shutdown_if_idle("{ \"if_idle\" : true }", 20), "if_idle: with spaces");
    check(!helper_shutdown_if_idle("{\"if_idle\":false}", 17), "if_idle: false");
    check(!helper_shutdown_if_idle("", 0) && !helper_shutdown_if_idle(NULL, 0), "if_idle: no body is a plain shutdown");
    check(!helper_shutdown_if_idle("{\"if_idle\":tr", 13), "if_idle: cut short is a plain shutdown");
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
    dedupe_units(dir);
    simulated_dedupe(dir, free_port());

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
