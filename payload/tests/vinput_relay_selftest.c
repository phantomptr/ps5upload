/* Host-side tests for the keyboard & mouse companion and the helper's relay to it:
 *   - the companion (vinput_companion.c) over a raw socket: the token, one helper at a time, a
 *     second instance, the heartbeat timeout letting go and removing the devices, the helper leaving
 *     ending it, idle exit, a device the backend could not make again asked for until it is back,
 *     the pid file written while it runs and removed only while it is its own;
 *   - the relay (vinput_relay.c) under vinput.c, against the real companion loop on a thread with a
 *     fake device ("launch" starts that thread, "kill" ends it): a session starts it ("starting"),
 *     input flows, a companion that dies or goes silent is killed (without the relay's lock) and
 *     started again with the session's devices made again, a companion that never gets through Init is not started again on its own (only a fresh open, never while it lives), no loader says so.
 * Build: see the root Makefile test-root (also under ASan and TSan). */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "vinput.h"
#include "vinput_companion.h"
#include "vinput_relay.h"
#include "vinput_wire.h"

static int failures = 0;
#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static char g_dir[128], g_token[192];
static int g_port;

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static void nap(int ms) {
    usleep((useconds_t)ms * 1000u);
}

/* ---- a fake device, shared with the companion thread ---- */

static pthread_mutex_t f_mu = PTHREAD_MUTEX_INITIALIZER;
static char f_log[16384];
static int f_can = 1;
static int f_fail_open; /* opens fail (AddDevice for a new user failing) */
static int f_remade;    /* the next tick says the devices were made again */

static void flog(const char *s) {
    pthread_mutex_lock(&f_mu);
    size_t n = strlen(f_log);
    if (n + strlen(s) + 2 < sizeof f_log) snprintf(f_log + n, sizeof f_log - n, "%s;", s);
    pthread_mutex_unlock(&f_mu);
}
static int fhas(const char *s) {
    int r;
    pthread_mutex_lock(&f_mu);
    r = strstr(f_log, s) != NULL;
    pthread_mutex_unlock(&f_mu);
    return r;
}
static void fclear(void) {
    pthread_mutex_lock(&f_mu);
    f_log[0] = 0;
    pthread_mutex_unlock(&f_mu);
}
static void d_caps(int *m, int *k) {
    *m = *k = f_can;
}
static int d_mopen(void) {
    flog("mopen");
    return __atomic_load_n(&f_fail_open, __ATOMIC_ACQUIRE) ? VINPUT_E_FAILED : VINPUT_OK;
}
static void d_mclose(void) {
    flog("mclose");
}
static int d_msend(uint8_t b, int dx, int dy, int w, int hw) {
    char s[64];
    snprintf(s, sizeof s, "m%u,%d,%d,%d,%d", b, dx, dy, w, hw);
    flog(s);
    return VINPUT_OK;
}
static int d_kopen(void) {
    flog("kopen");
    return __atomic_load_n(&f_fail_open, __ATOMIC_ACQUIRE) ? VINPUT_E_FAILED : VINPUT_OK;
}
static int d_tick(void) {
    return __atomic_exchange_n(&f_remade, 0, __ATOMIC_ACQ_REL);
}
static void d_kclose(void) {
    flog("kclose");
}
static int d_ksend(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    char s[64];
    snprintf(s, sizeof s, "k%u,%u,%u", mods, keys[0], keys[1]);
    flog(s);
    return VINPUT_OK;
}
static const vinput_backend_t DEV = {"fake", d_caps, d_mopen, d_mclose, d_msend, d_kopen, d_kclose, d_ksend, NULL};

/* ---- companions on threads ---- */

typedef struct {
    pthread_t th;
    volatile int stop, hang, done, code;
    vcomp_cfg_t cfg;
} comp_t;

static void *comp_main(void *arg) {
    comp_t *c = arg;
    __atomic_store_n(&c->code, vcomp_run(&c->cfg), __ATOMIC_RELEASE);
    __atomic_store_n(&c->done, 1, __ATOMIC_RELEASE);
    return NULL;
}

static pthread_mutex_t l_mu = PTHREAD_MUTEX_INITIALIZER;
static comp_t *g_comp;
static int l_mode; /* 0 start a companion, 1 no loader, 2 sent but nothing starts */
static int l_launches, l_kills;
static char l_pidfile[256]; /* mode 2 writes its pid here, as a companion does before its Init */
static const char *g_comp_pid = ""; /* the pid file of the next comp_start */

#define DONE(c) __atomic_load_n(&(c)->done, __ATOMIC_ACQUIRE)

static int kills(void) {
    int n;
    pthread_mutex_lock(&l_mu);
    n = l_kills;
    pthread_mutex_unlock(&l_mu);
    return n;
}

static int launches(void) {
    int n;
    pthread_mutex_lock(&l_mu);
    n = l_launches;
    pthread_mutex_unlock(&l_mu);
    return n;
}

static comp_t *comp_start(long long hb_ms, long long idle_ms) {
    comp_t *c = calloc(1, sizeof *c);
    c->cfg.port = g_port;
    c->cfg.token_path = g_token;
    c->cfg.pid_path = g_comp_pid;
    c->cfg.dev_tick = d_tick;
    c->cfg.hb_timeout_ms = hb_ms;
    c->cfg.idle_exit_ms = idle_ms;
    c->cfg.auth_wait_ms = 3000;
    c->cfg.dev = &DEV;
    c->cfg.stop = &c->stop;
    c->cfg.hang = &c->hang;
    pthread_create(&c->th, NULL, comp_main, c);
    return c;
}

static void comp_end(comp_t *c) {
    if (!c) return;
    __atomic_store_n(&c->stop, 1, __ATOMIC_RELEASE);
    pthread_join(c->th, NULL);
    free(c);
}

static void write_token(const uint8_t tok[VWIRE_TOKEN_LEN]) {
    char hex[VWIRE_TOKEN_HEX + 1];
    FILE *f = fopen(g_token, "w");
    vwire_token_to_hex(tok, hex);
    fputs(hex, f);
    fclose(f);
}

static int dial(void) {
    struct sockaddr_in a;
    int fd;
    for (int i = 0; i < 100; i++) {
        fd = socket(AF_INET, SOCK_STREAM, 0);
        memset(&a, 0, sizeof a);
        a.sin_family = AF_INET;
        a.sin_port = htons((uint16_t)g_port);
        a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (connect(fd, (struct sockaddr *)&a, sizeof a) == 0) {
            struct timeval to = {2, 0};
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);
            return fd;
        }
        close(fd);
        nap(20);
    }
    return -1;
}

/* One request on a raw connection: the rc (and payload after it), or -100. */
static int req(int fd, uint8_t type, const void *p, size_t n, uint8_t *out) {
    static uint8_t seq;
    uint8_t b[VWIRE_MAX_FRAME], r[VWIRE_MAX_FRAME];
    size_t have = 0;
    vwire_frame_t f;
    int k = vwire_encode(b, sizeof b, type, ++seq, p, n);
    if (send(fd, b, (size_t)k, 0) != k) return -100;
    for (;;) {
        ssize_t got = recv(fd, r + have, sizeof r - have, 0);
        int used;
        if (got <= 0) return -100;
        have += (size_t)got;
        used = vwire_decode(r, have, &f);
        if (used > 0) {
            if (out) memcpy(out, f.payload + 4, f.len - 4u);
            return (int)(int32_t)vwire_get32(f.payload);
        }
    }
}

static void test_wire(void) {
    uint8_t b[VWIRE_MAX_FRAME], tok[VWIRE_TOKEN_LEN], back[VWIRE_TOKEN_LEN];
    vwire_frame_t f;
    char hex[VWIRE_TOKEN_HEX + 1];
    CHECK(vwire_encode(b, sizeof b, VWIRE_OPEN, 9, "\x01\x00", 2) == 6);
    CHECK(vwire_decode(b, 5, &f) == 0);
    CHECK(vwire_decode(b, 6, &f) == 6 && f.type == VWIRE_OPEN && f.seq == 9 && f.len == 2 && f.payload[0] == 1);
    b[2] = 0xff;
    CHECK(vwire_decode(b, 6, &f) == -1);
    CHECK(vwire_encode(b, sizeof b, 1, 1, b, VWIRE_MAX_PAYLOAD + 1) == -1);
    for (int i = 0; i < VWIRE_TOKEN_LEN; i++) tok[i] = (uint8_t)(i * 7 + 1);
    vwire_token_to_hex(tok, hex);
    CHECK(vwire_token_from_hex(hex, strlen(hex), back) == 0 && vwire_token_equal(tok, back));
    back[31] ^= 1;
    CHECK(!vwire_token_equal(tok, back));
    hex[3] = 'z';
    CHECK(vwire_token_from_hex(hex, strlen(hex), back) == -1);
}

static long pid_in(const char *path) {
    char s[24] = {0};
    FILE *f = fopen(path, "r");
    size_t n;
    if (!f) return 0;
    n = fread(s, 1, sizeof s - 1, f);
    fclose(f);
    s[n] = 0;
    return strtol(s, NULL, 10);
}

static void test_companion(void) {
    char pidf[256];
    uint8_t tok[VWIRE_TOKEN_LEN], hello[2 + VWIRE_TOKEN_LEN], out[VWIRE_MAX_PAYLOAD], rep[VINPUT_REPORT_LEN] = {0};
    comp_t *c, *c2;
    int fd, fd2;
    vcomp_cfg_t nt;
    /* no token: not started by a helper */
    unlink(g_token);
    memset(&nt, 0, sizeof nt);
    nt.port = g_port;
    nt.token_path = g_token;
    nt.pid_path = "";
    nt.dev = &DEV;
    CHECK(vcomp_run(&nt) == VCOMP_EXIT_NO_TOKEN);

    for (int i = 0; i < VWIRE_TOKEN_LEN; i++) tok[i] = (uint8_t)(0xa0 + i);
    write_token(tok);
    fclear();
    c = comp_start(300, 100000);
    fd = dial();
    CHECK(fd >= 0);
    {
        struct stat st;
        nap(50);
        CHECK(stat(g_token, &st) != 0); /* read and deleted */
    }
    /* a second instance cannot bind */
    write_token(tok);
    CHECK(vcomp_run(&nt) == VCOMP_EXIT_BUSY);
    unlink(g_token);
    /* a wrong token is refused and the connection closed */
    vwire_put16(hello, VWIRE_VERSION);
    memset(hello + 2, 0x55, VWIRE_TOKEN_LEN);
    CHECK(req(fd, VWIRE_HELLO, hello, sizeof hello, out) == VWIRE_RC_BAD_TOKEN);
    close(fd);
    /* the right one */
    fd = dial();
    memcpy(hello + 2, tok, VWIRE_TOKEN_LEN);
    CHECK(req(fd, VWIRE_HELLO, hello, sizeof hello, out) == VWIRE_RC_OK);
    CHECK(vwire_get16(out) == VWIRE_VERSION && (int)vwire_get32(out + 2) == (int)getpid() && out[6] == 1 && out[7] == 1);
    /* a second helper is turned away while the first is connected */
    fd2 = dial();
    {
        char x;
        CHECK(recv(fd2, &x, 1, 0) == 0);
    }
    close(fd2);
    CHECK(req(fd, VWIRE_OPEN, "\x01\x01", 2, out) == VWIRE_RC_OK && out[0] == 1 && out[1] == 1);
    rep[0] = 1;
    rep[2] = 5;
    rep[8] = 4;
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_OK);
    CHECK(fhas("m1,5,0,0,0") && fhas("k0,4,0"));
    /* motion-free same state: nothing sent again */
    fclear();
    rep[2] = 0;
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_OK);
    CHECK(!fhas("m1") && !fhas("k0,4"));
    CHECK(req(fd, VWIRE_REPORT, rep, 3, NULL) == VWIRE_RC_BAD_REQUEST);
    /* OPEN without the keyboard lets it go and removes it */
    CHECK(req(fd, VWIRE_OPEN, "\x01\x00", 2, out) == VWIRE_RC_OK && out[0] == 1 && out[1] == 0);
    CHECK(fhas("k0,0,0;kclose"));
    CHECK(req(fd, VWIRE_OPEN, "\x01\x01", 2, out) == VWIRE_RC_OK);
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_OK);
    /* silence past the heartbeat timeout: everything let go, devices removed, connection kept */
    fclear();
    nap(600);
    CHECK(fhas("m0,0,0,0,0;mclose") && fhas("k0,0,0;kclose"));
    CHECK(req(fd, VWIRE_CAPS, NULL, 0, out) == VWIRE_RC_OK && out[2] == 0 && out[3] == 0);
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_FAILED);
    /* the helper leaving ends it, letting go first */
    CHECK(req(fd, VWIRE_OPEN, "\x01\x01", 2, out) == VWIRE_RC_OK);
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_OK);
    fclear();
    close(fd);
    for (int i = 0; i < 100 && !DONE(c); i++) nap(10);
    CHECK(DONE(c) && __atomic_load_n(&c->code, __ATOMIC_ACQUIRE) == VCOMP_EXIT_DONE);
    CHECK(fhas("m0,0,0,0,0;mclose") && fhas("kclose"));
    comp_end(c);

    /* idle: no device for idle_exit_ms ends it; its own pid file is removed */
    snprintf(pidf, sizeof pidf, "%s/vinput.pid", g_dir);
    write_token(tok);
    g_comp_pid = pidf;
    c2 = comp_start(300, 200);
    g_comp_pid = "";
    fd = dial();
    CHECK(req(fd, VWIRE_HELLO, hello, sizeof hello, out) == VWIRE_RC_OK);
    CHECK(pid_in(pidf) == (long)getpid());
    for (int i = 0; i < 100 && !DONE(c2); i++) nap(10);
    CHECK(DONE(c2));
    CHECK(access(pidf, F_OK) != 0);
    close(fd);
    comp_end(c2);
}

static int caps_open(int fd, int m, int k) {
    uint8_t out[VWIRE_MAX_PAYLOAD];
    return req(fd, VWIRE_CAPS, NULL, 0, out) == VWIRE_RC_OK && out[2] == m && out[3] == k;
}

/* A new foreground user and AddDevice failing: the devices are not there, input says so, and they
 * are asked for again every tick until they are back. A pid file that is no longer its own stays. */
static void test_companion_remake(void) {
    uint8_t tok[VWIRE_TOKEN_LEN], hello[2 + VWIRE_TOKEN_LEN], out[VWIRE_MAX_PAYLOAD], rep[VINPUT_REPORT_LEN] = {0};
    char pidf[256];
    comp_t *c;
    int fd, ok;
    for (int i = 0; i < VWIRE_TOKEN_LEN; i++) tok[i] = (uint8_t)(0x30 + i);
    vwire_put16(hello, VWIRE_VERSION);
    memcpy(hello + 2, tok, VWIRE_TOKEN_LEN);
    snprintf(pidf, sizeof pidf, "%s/vinput.pid", g_dir);
    write_token(tok);
    g_comp_pid = pidf;
    c = comp_start(5000, 100000);
    g_comp_pid = "";
    fd = dial();
    CHECK(req(fd, VWIRE_HELLO, hello, sizeof hello, out) == VWIRE_RC_OK);
    CHECK(req(fd, VWIRE_OPEN, "\x01\x01", 2, out) == VWIRE_RC_OK && out[0] == 1 && out[1] == 1);
    __atomic_store_n(&f_fail_open, 1, __ATOMIC_RELEASE);
    __atomic_store_n(&f_remade, 1, __ATOMIC_RELEASE);
    ok = 0;
    for (int i = 0; i < 30 && !ok; i++) {
        nap(100);
        ok = caps_open(fd, 0, 0);
    }
    CHECK(ok);
    rep[0] = 1;
    rep[2] = 5;
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_FAILED);
    /* an OPEN asks again (and still fails) */
    fclear();
    CHECK(req(fd, VWIRE_OPEN, "\x01\x01", 2, out) == VWIRE_RC_FAILED && out[0] == 0 && out[1] == 0);
    CHECK(fhas("mopen") && fhas("kopen"));
    /* AddDevice works again: the next tick makes them, input flows */
    fclear();
    __atomic_store_n(&f_fail_open, 0, __ATOMIC_RELEASE);
    ok = 0;
    for (int i = 0; i < 30 && !ok; i++) {
        nap(100);
        ok = caps_open(fd, 1, 1);
    }
    CHECK(ok);
    CHECK(fhas("mopen") && fhas("kopen"));
    CHECK(req(fd, VWIRE_REPORT, rep, sizeof rep, NULL) == VWIRE_RC_OK);
    CHECK(fhas("m1,5,0,0,0"));
    /* another companion's pid in the file: left alone at exit */
    {
        FILE *f = fopen(pidf, "w");
        if (f) {
            fprintf(f, "%d\n", 1);
            fclose(f);
        }
    }
    close(fd);
    for (int i = 0; i < 100 && !DONE(c); i++) nap(10);
    CHECK(DONE(c));
    CHECK(pid_in(pidf) == 1);
    unlink(pidf);
    comp_end(c);
}

/* ---- the relay ---- */

static int fake_launch(void *ctx) {
    (void)ctx;
    pthread_mutex_lock(&l_mu);
    l_launches++;
    if (l_mode == 0) {
        if (g_comp && DONE(g_comp)) {
            comp_end(g_comp);
            g_comp = NULL;
        }
        if (!g_comp) g_comp = comp_start(400, 100000);
    }
    if (l_mode == 2 && l_pidfile[0]) {
        FILE *f = fopen(l_pidfile, "w");
        if (f) {
            fprintf(f, "%d\n", (int)getpid());
            fclose(f);
        }
    }
    pthread_mutex_unlock(&l_mu);
    return l_mode == 1 ? VRELAY_NO_LOADER : 0;
}

static void *relay_lock_probe(void *arg) {
    (void)vinput_relay_pid(); /* takes the relay's lock */
    __atomic_store_n((int *)arg, 1, __ATOMIC_RELEASE);
    return NULL;
}

static void fake_kill(void *ctx, int pid) {
    static int got;
    pthread_t t;
    (void)ctx;
    CHECK(pid == (int)getpid());
    /* Called without the relay's lock: the console's kill waits for the companion to let go, and
     * sessions must be answered meanwhile. */
    __atomic_store_n(&got, 0, __ATOMIC_RELEASE);
    if (pthread_create(&t, NULL, relay_lock_probe, &got) == 0) {
        for (int i = 0; i < 50 && !__atomic_load_n(&got, __ATOMIC_ACQUIRE); i++) nap(10);
        CHECK(__atomic_load_n(&got, __ATOMIC_ACQUIRE));
        if (__atomic_load_n(&got, __ATOMIC_ACQUIRE))
            pthread_join(t, NULL);
        else
            pthread_detach(t);
    }
    pthread_mutex_lock(&l_mu);
    l_kills++;
    comp_end(g_comp);
    g_comp = NULL;
    pthread_mutex_unlock(&l_mu);
}

static void relay_setup(int mode) {
    vrelay_cfg_t cfg;
    vinput_set_backend(NULL);
    vinput_relay_reset();
    pthread_mutex_lock(&l_mu);
    if (g_comp) comp_end(g_comp);
    g_comp = NULL;
    l_mode = mode;
    l_launches = l_kills = 0;
    pthread_mutex_unlock(&l_mu);
    memset(&cfg, 0, sizeof cfg);
    cfg.launch = fake_launch;
    cfg.kill = fake_kill;
    cfg.port = g_port;
    cfg.token_path = g_token;
    cfg.pid_path = "/nonexistent/vinput.pid";
    cfg.call_timeout_ms = 100;
    cfg.silent_ms = 300;
    cfg.hb_ms = 50;
    cfg.connect_wait_ms = 600;
    cfg.max_fails = 3;
    cfg.backoff_ms = 50;
    cfg.healthy_ms = 100000;
    cfg.loader_retry_ms = 300;
    vinput_relay_configure(&cfg);
    vinput_set_backend(vinput_relay_backend());
    fclear();
}

static char g_out[VINPUT_OUT_MAX];

static int session(const char *body) {
    return vinput_session_json(body, strlen(body), vinput_now_ms(), g_out, sizeof g_out);
}
static int report_hex(const char *hex) {
    char body[128];
    snprintf(body, sizeof body, "{\"id\":7,\"r\":\"%s\"}", hex);
    return vinput_report_json(body, strlen(body), vinput_now_ms(), g_out, sizeof g_out);
}
static int has(const char *s) {
    return strstr(g_out, s) != NULL;
}

/* Opens until it answers ok (the companion starting meanwhile). 1 when it did within ms. */
static int open_until_ok(int ms, int *saw_starting) {
    long long until = now_ms() + ms;
    while (now_ms() < until) {
        session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
        if (has("\"ok\":true")) return 1;
        if (has("vinput_starting") && saw_starting) *saw_starting = 1;
        nap(20);
    }
    return 0;
}

static int wait_for(int (*cond)(void), int ms) {
    long long until = now_ms() + ms;
    while (now_ms() < until) {
        if (cond()) return 1;
        nap(10);
        session("{\"op\":\"hb\",\"id\":7}");
    }
    return cond();
}

static int report_ok(void) {
    report_hex("01000500000000000400000000000000");
    return has("\"ok\":true");
}
static int caps_has_running(void) {
    vinput_caps_json(g_out, sizeof g_out);
    return has("\"companion\":\"running\"");
}

static void test_relay(void) {
    int starting = 0;
    relay_setup(0);
    CHECK(vinput_caps_json(g_out, sizeof g_out) > 0);
    CHECK(has("\"mouse\":true") && has("\"backend\":\"relay\"") && has("\"companion\":\"idle\"") && !has("reason"));
    /* the first open starts it */
    session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("\"ok\":false") && has("\"err\":\"vinput_starting\"") && has("\"open\":false"));
    CHECK(open_until_ok(3000, &starting));
    CHECK(launches() == 1);
    CHECK(vinput_relay_pid() == (int)getpid());
    CHECK(caps_has_running());
    CHECK(fhas("mopen") && fhas("kopen"));
    /* input flows */
    CHECK(report_ok());
    CHECK(fhas("m1,5,0,0,0") && fhas("k0,4,0"));
    /* heartbeats from the relay keep the companion's devices well past its 400 ms timeout */
    fclear();
    for (int i = 0; i < 8; i++) {
        nap(100);
        session("{\"op\":\"hb\",\"id\":7}");
    }
    CHECK(!fhas("mclose"));
    /* close lets go and removes */
    session("{\"op\":\"close\",\"id\":7}");
    CHECK(has("\"ok\":true"));
    CHECK(fhas("m0,0,0,0,0") && fhas("mclose") && fhas("kclose"));

    /* the companion dies mid-session: started again, the devices made again, input flows */
    CHECK(open_until_ok(2000, NULL));
    CHECK(report_ok());
    fclear();
    pthread_mutex_lock(&l_mu);
    comp_end(g_comp); /* killed from outside: the connection closes */
    g_comp = NULL;
    pthread_mutex_unlock(&l_mu);
    nap(150);
    session("{\"op\":\"hb\",\"id\":7}");
    CHECK(has("\"ok\":true")); /* the session stays */
    CHECK(wait_for(caps_has_running, 3000));
    CHECK(launches() == 2);
    CHECK(wait_for(report_ok, 2000));
    CHECK(fhas("mopen") && fhas("kopen") && fhas("m1,5,0,0,0"));

    /* the companion goes silent: killed by pid after silent_ms, started again */
    pthread_mutex_lock(&l_mu);
    __atomic_store_n(&g_comp->hang, 1, __ATOMIC_RELEASE);
    pthread_mutex_unlock(&l_mu);
    {
        long long t0 = now_ms();
        while (now_ms() - t0 < 2000 && kills() == 0) {
            report_hex("00000100000000000000000000000000");
            nap(20);
        }
        CHECK(kills() == 1);
        CHECK(now_ms() - t0 < 1500);
    }
    CHECK(wait_for(caps_has_running, 3000));
    CHECK(launches() == 3);
    CHECK(wait_for(report_ok, 2000));
    session("{\"op\":\"close\",\"id\":7}");

    /* the helper stopping lets go and the relay's connection, when dropped, ends the companion */
    CHECK(open_until_ok(2000, NULL));
    CHECK(report_ok());
    fclear();
    vinput_stop();
    CHECK(fhas("mclose") && fhas("kclose"));
    vinput_relay_reset();
    {
        int done = 0;
        for (int i = 0; i < 100 && !done; i++) {
            pthread_mutex_lock(&l_mu);
            done = g_comp && DONE(g_comp);
            pthread_mutex_unlock(&l_mu);
            nap(10);
        }
        CHECK(done);
    }
}

static void test_relay_no_loader(void) {
    relay_setup(1);
    session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("vinput_starting"));
    nap(200);
    CHECK(vinput_caps_json(g_out, sizeof g_out) > 0);
    CHECK(has("\"companion\":\"unavailable\"") && has("\"reason\":\"vinput_needs_loader\"") && has("\"mouse\":false"));
    session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("\"err\":\"vinput_needs_loader\""));
    CHECK(launches() == 1);
    /* asked again after loader_retry_ms (a loader may have been started since) */
    nap(350);
    session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("vinput_starting"));
    nap(200);
    CHECK(launches() == 2);
}

/* A companion that never gets through Init (mode 2: the loader took it, nothing answers; on an
 * untested firmware an Init that hangs, maybe unkillably): ended by pid, and NOT started again on its
 * own, neither by the engine's quick "starting" re-asks nor after a while. A fresh open (the mode
 * turned on again) tries once more, but never while the stuck one still lives. */
static int f_stuck_alive; /* the fake's answer for the stuck pid */
static int fake_alive(void *ctx, int pid) {
    (void)ctx;
    (void)pid;
    return __atomic_load_n(&f_stuck_alive, __ATOMIC_ACQUIRE);
}

static void test_relay_init_failure_not_retried(void) {
    char pidf[256];
    vrelay_cfg_t cfg;
    long long until;
    relay_setup(2);
    snprintf(pidf, sizeof pidf, "%s/vinput.pid", g_dir);
    pthread_mutex_lock(&l_mu);
    snprintf(l_pidfile, sizeof l_pidfile, "%s", pidf);
    pthread_mutex_unlock(&l_mu);
    vinput_set_backend(NULL);
    vinput_relay_reset();
    memset(&cfg, 0, sizeof cfg);
    cfg.launch = fake_launch;
    cfg.kill = fake_kill;
    cfg.alive = fake_alive;
    cfg.port = g_port;
    cfg.token_path = g_token;
    cfg.pid_path = pidf;
    cfg.call_timeout_ms = 100;
    cfg.silent_ms = 300;
    cfg.hb_ms = 50;
    cfg.connect_wait_ms = 400;
    cfg.max_fails = 3;
    cfg.backoff_ms = 50;
    cfg.healthy_ms = 300; /* the old "asked again after" wait: must not apply */
    cfg.loader_retry_ms = 300;
    vinput_relay_configure(&cfg);
    vinput_set_backend(vinput_relay_backend());
    __atomic_store_n(&f_stuck_alive, 0, __ATOMIC_RELEASE);

    /* the engine's open, re-asked every 300 ms while it answers "starting" */
    session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("vinput_starting"));
    until = now_ms() + 3000;
    while (now_ms() < until && has("vinput_starting")) {
        nap(300);
        session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
    }
    CHECK(has("\"err\":\"vinput_companion_failed\""));
    CHECK(launches() == 1); /* one start, no backoff restarts */
    vinput_caps_json(g_out, sizeof g_out);
    CHECK(has("\"companion\":\"unavailable\"") && has("\"reason\":\"vinput_companion_failed\""));
    /* well past every old retry wait, with quick asks: still not started again */
    for (int i = 0; i < 6; i++) {
        nap(150);
        session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}");
        CHECK(has("\"err\":\"vinput_companion_failed\""));
    }
    nap(400);
    CHECK(launches() == 1);
    CHECK(kills() == 1); /* the one in Init was ended by its pid */

    /* a fresh open (asks stopped for > 2 s), while the stuck one still lives: not started */
    __atomic_store_n(&f_stuck_alive, 1, __ATOMIC_RELEASE);
    nap(2100);
    session("{\"op\":\"open\",\"id\":8,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("\"err\":\"vinput_companion_failed\""));
    nap(300);
    CHECK(launches() == 1);

    /* it is gone: the next fresh open tries once more (and only once) */
    __atomic_store_n(&f_stuck_alive, 0, __ATOMIC_RELEASE);
    nap(2100);
    session("{\"op\":\"open\",\"id\":9,\"mouse\":true,\"keyboard\":true}");
    CHECK(has("vinput_starting"));
    until = now_ms() + 3000;
    while (now_ms() < until && has("vinput_starting")) {
        nap(300);
        session("{\"op\":\"open\",\"id\":9,\"mouse\":true,\"keyboard\":true}");
    }
    CHECK(has("\"err\":\"vinput_companion_failed\""));
    nap(500);
    CHECK(launches() == 2);

    /* and a fresh open on a console where it works: up again */
    pthread_mutex_lock(&l_mu);
    l_mode = 0;
    pthread_mutex_unlock(&l_mu);
    nap(2100);
    CHECK(open_until_ok(3000, NULL));
    CHECK(launches() == 3);
    session("{\"op\":\"close\",\"id\":9}");
    pthread_mutex_lock(&l_mu);
    l_pidfile[0] = '\0';
    pthread_mutex_unlock(&l_mu);
    unlink(pidf);
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "/tmp/ps5u-vinput-test-%d", (int)getpid());
    mkdir(g_dir, 0700);
    snprintf(g_token, sizeof g_token, "%s/vinput.token", g_dir);
    g_port = 30000 + (int)(getpid() % 20000);
    vinput_test_no_watchdog(1);

    test_wire();
    test_companion();
    test_companion_remake();
    test_relay();
    test_relay_no_loader();
    test_relay_init_failure_not_retried();

    vinput_set_backend(NULL);
    vinput_relay_reset();
    pthread_mutex_lock(&l_mu);
    if (g_comp) comp_end(g_comp);
    pthread_mutex_unlock(&l_mu);
    unlink(g_token);
    rmdir(g_dir);
    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("vinput relay selftest: ok\n");
    return 0;
}
