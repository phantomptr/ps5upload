#include "ava1_data.h"

#include <errno.h>
#include <stdarg.h>
#include <fcntl.h>
#include <sys/resource.h>
#include <unistd.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_apply.h"
#include "ava1_copy.h"
#include "ava1_op.h"
#include "ava1_events.h"
#include "ava1_frame.h"
#include "ava1_job.h"
#include "ava1_platform.h"
#include "ava1_recv.h"
#include "ava1_send.h"
#include "ava1_thread.h"

#define IN_MAX (32u << 20)   /* control bytes one job's inbox holds */
#define OPEN_MAX (16u << 20) /* control bytes queued behind one JobOpen still opening */

uint32_t ava1_data_test_open_delay_ms;
uint32_t ava1_data_test_open_work_delay_ms;
uint32_t ava1_data_test_map_delay_ms;
int ava1_data_test_ack_fail;
int ava1_data_test_feeder_fail;
uint32_t ava1_data_test_feed_delay_ms;
int ava1_data_test_reserve_fail;
int ava1_data_test_lane_alloc_fail;
int ava1_data_test_fb_force;

/* Tests only: the slow work behind a JobOpen waits this long, so a test can land a
 * pipelined cap refusal inside the open's window (between the two refusal checks). */
void ava1_test_set_open_work_delay_ms(uint32_t ms) { ava1_data_test_open_work_delay_ms = ms; }

static struct {
    ava1_data_cfg_t cfg;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    uint64_t budget_free;
    uint64_t admitted; /* lane bytes between their header and on_lane */
    int bg;            /* ava1_data_spawn threads running */
    int fb;            /* Received waiting-sends spawned (bounded, see RECV_FB_MAX) */
    volatile int running;
    pthread_t house, recover;
    int boot_recovered; /* the start-time recovery pass has run (recover_main) */
} D = { .mu = PTHREAD_MUTEX_INITIALIZER, .cv = PTHREAD_COND_INITIALIZER };

const ava1_data_cfg_t *ava1_data_cfg(void) { return &D.cfg; }

uint64_t ava1_budget_take(uint64_t want, uint64_t min) {
    uint64_t g;
    pthread_mutex_lock(&D.mu);
    g = want < D.budget_free ? want : D.budget_free;
    if (g < min) g = 0;
    D.budget_free -= g;
    pthread_mutex_unlock(&D.mu);
    return g;
}

void ava1_budget_give(uint64_t n) {
    pthread_mutex_lock(&D.mu);
    D.budget_free += n;
    pthread_mutex_unlock(&D.mu);
}

typedef struct {
    void *(*fn)(void *);
    void *arg;
} bg_t;

static void *bg_main(void *p) {
    bg_t b = *(bg_t *)p;
    free(p);
    b.fn(b.arg);
    pthread_mutex_lock(&D.mu);
    D.bg--;
    pthread_cond_broadcast(&D.cv);
    pthread_mutex_unlock(&D.mu);
    return NULL;
}

int ava1_data_spawn(void *(*fn)(void *), void *arg) {
    bg_t *b = malloc(sizeof *b);
    if (!b) return -1;
    b->fn = fn;
    b->arg = arg;
    pthread_mutex_lock(&D.mu);
    D.bg++;
    pthread_mutex_unlock(&D.mu);
    if (ava1_thread_start(bg_main, b, NULL) != 0) {
        free(b);
        pthread_mutex_lock(&D.mu);
        D.bg--;
        pthread_cond_broadcast(&D.cv);
        pthread_mutex_unlock(&D.mu);
        return -1;
    }
    return 0;
}

static uint64_t g_unswept_total;
void ava1_unswept_add(int64_t delta) {
    if (delta >= 0) {
        __atomic_add_fetch(&g_unswept_total, (uint64_t)delta, __ATOMIC_RELAXED);
    } else { /* never below zero, whatever the order the claims and releases land in */
        uint64_t cur = __atomic_load_n(&g_unswept_total, __ATOMIC_RELAXED), sub = (uint64_t)(-delta), want;
        do {
            want = cur >= sub ? cur - sub : 0;
        } while (!__atomic_compare_exchange_n(&g_unswept_total, &cur, want, 0, __ATOMIC_RELAXED, __ATOMIC_RELAXED));
    }
}
uint64_t ava1_unswept_total(void) { return __atomic_load_n(&g_unswept_total, __ATOMIC_RELAXED); }

/* 1 once the start-time recovery pass (recover_main) has run; a JobOpen for a job with a log is BUSY before. */
int ava1_data_boot_recovered(void) { return __atomic_load_n(&D.boot_recovered, __ATOMIC_SEQ_CST); }

int ava1_data_running(void) { return __atomic_load_n(&D.running, __ATOMIC_RELAXED); }

unsigned ava1_house_ticks;

static void *house_main(void *arg) {
    (void)arg;
    while (D.running) {
        __atomic_add_fetch(&ava1_house_ticks, 1, __ATOMIC_RELAXED);
        ava1_job_reap(ava1_mono_ms());
        ava1_platform_sleep_ms(100);
    }
    return NULL;
}

/* A reaped settling job, or one a crash left, has its log recovered here: the only copy of its files must
 * never wait for the next helper start. A thread of its own, so a slow recovery (a sweep that keeps failing,
 * a large log) never delays the reaper. */
static void *recover_main(void *arg) {
    uint64_t last;
    (void)arg;
    /* A helper that died (or was stopped) with files not yet durable in place finishes them now: the
     * start's pass, here and not on the thread that starts the listeners (a large log made the main
     * thread, and with it the legacy ports and AVA1, wait). Until it ends a JobOpen for a job that holds
     * a log is answered BUSY (the sender retries), so no session takes one before it is recovered. */
    if (D.cfg.jobs_dir[0]) (void)ava1_recv_recover_pass(D.cfg.jobs_dir, D.cfg.recover_max);
    __atomic_store_n(&D.boot_recovered, 1, __ATOMIC_SEQ_CST);
    last = ava1_mono_ms();
    while (D.running) {
        uint64_t now = ava1_mono_ms();
        if (D.cfg.jobs_dir[0] && now - last >= D.cfg.recover_every_ms) {
            (void)ava1_recv_recover_pass(D.cfg.jobs_dir, D.cfg.recover_max);
            last = ava1_mono_ms();
        }
        ava1_platform_sleep_ms(50);
    }
    return NULL;
}

uint32_t ava1_data_test_fd_budget;
uint32_t ava1_data_test_cal_peak;
static uint32_t g_fd_budget = 512 - 128;
static uint32_t g_pend_open, g_pend_peak;
static int g_fd_logged;

/* RLIMIT_NOFILE is not always the real ceiling (a console's kernel tables can stop
 * earlier): open /dev/null until something refuses. Returns how many opened. */
#define FD_PROBE_MAX 4096
static uint32_t fd_probe(int *stop_errno) {
    int *fds = malloc(FD_PROBE_MAX * sizeof *fds);
    uint32_t n = 0;
    *stop_errno = 0;
    if (!fds) { *stop_errno = ENOMEM; return 0; }
    while (n < FD_PROBE_MAX) {
        int fd = open("/dev/null", O_RDONLY);
        if (fd < 0) { *stop_errno = errno; break; }
        fds[n++] = fd;
    }
    {
        uint32_t i;
        for (i = 0; i < n; i++) close(fds[i]);
    }
    free(fds);
    return n;
}

static int g_fd_probed;

static void fd_limit_init(void) {
    struct rlimit rl;
    /* Once per process: the probe below opens descriptors until the kernel refuses, and any other thread
     * opening or accepting in that window fails. ava1_fd_limits_probe() runs it early, before the listeners
     * have threads; a later data-layer start reuses the answer. */
    if (g_fd_probed) return;
    g_fd_probed = 1;
    uint64_t soft = 512;
    int have = getrlimit(RLIMIT_NOFILE, &rl) == 0;
    if (have) {
        rlim_t hard = rl.rlim_max, want = rl.rlim_cur;
        if (!g_fd_logged)
            fprintf(stderr, "[ava1] RLIMIT_NOFILE soft=%llu hard=%llu\n", (unsigned long long)rl.rlim_cur,
                    (unsigned long long)rl.rlim_max);
        {
            struct rlimit up = rl;
            up.rlim_cur = hard > 65536 ? 65536 : hard;
            if (up.rlim_cur > rl.rlim_cur) {
                int ok = setrlimit(RLIMIT_NOFILE, &up) == 0;
                if (ok) want = up.rlim_cur;
                if (!g_fd_logged)
                    fprintf(stderr, "[ava1] raising RLIMIT_NOFILE soft to %llu: %s\n", (unsigned long long)up.rlim_cur,
                            ok ? "ok" : strerror(errno));
            }
        }
        if (getrlimit(RLIMIT_NOFILE, &rl) == 0) want = rl.rlim_cur;
        soft = want == RLIM_INFINITY || want > 1000000 ? 1000000 : (uint64_t)want;
    } else if (!g_fd_logged) {
        fprintf(stderr, "[ava1] getrlimit(RLIMIT_NOFILE) failed: %s; assuming 512\n", strerror(errno));
    }
    g_fd_budget = soft > 144 ? (uint32_t)(soft - 128) : 16;
    {
        int e;
        uint32_t got = fd_probe(&e);
        uint32_t cap = got > 144 ? got - 128 : 16;
        if (!g_fd_logged)
            fprintf(stderr, "[ava1] fd probe: %u opened, stopped by %s%s\n", got, e ? strerror(e) : "the probe bound",
                    e ? "" : " (4096)");
        if (e && cap < g_fd_budget) g_fd_budget = cap;
        else if (!e && got == FD_PROBE_MAX && g_fd_budget > FD_PROBE_MAX - 128) g_fd_budget = FD_PROBE_MAX - 128;
    }
    if (!g_fd_logged) fprintf(stderr, "[ava1] open-file budget %u\n", g_fd_budget);
    g_fd_logged = 1;
}

void ava1_fd_limits_probe(void) { fd_limit_init(); }

uint32_t ava1_fd_budget(void) {
    uint32_t t = __atomic_load_n(&ava1_data_test_fd_budget, __ATOMIC_SEQ_CST);
    return t ? t : g_fd_budget;
}

uint32_t ava1_pend_share(void) {
    uint32_t s = ava1_fd_budget() / 2;
    return s < 4 ? 4 : s;
}

int ava1_pend_full(void) { return __atomic_load_n(&g_pend_open, __ATOMIC_SEQ_CST) >= ava1_pend_share(); }

int ava1_pend_reserve(int (*stopping)(void *), void (*idle)(void *), void *arg) {
    for (;;) {
        uint32_t cur = __atomic_load_n(&g_pend_open, __ATOMIC_SEQ_CST);
        while (cur < ava1_pend_share()) {
            if (__atomic_compare_exchange_n(&g_pend_open, &cur, cur + 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
                uint32_t pk = __atomic_load_n(&g_pend_peak, __ATOMIC_SEQ_CST);
                while (cur + 1 > pk &&
                       !__atomic_compare_exchange_n(&g_pend_peak, &pk, cur + 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {}
                return 1;
            }
        }
        if (stopping && stopping(arg)) return 0;
        if (idle) idle(arg);
        else ava1_platform_sleep_ms(2);
    }
}

void ava1_pend_release(uint32_t n) {
    uint32_t cur = __atomic_load_n(&g_pend_open, __ATOMIC_SEQ_CST);
    while (n) {
        uint32_t take = cur < n ? cur : n;
        if (__atomic_compare_exchange_n(&g_pend_open, &cur, cur - take, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) break;
    }
}

/* Descriptors held by large files (each open one holds a part file and, from two groups up, an
 * outboard) until the file commits. They used to be uncapped: a first group that spanned thousands
 * of files kept two descriptors per file and used the whole table (about 600 on the console), so
 * every accept() and open() failed (final review: console). A quarter of the budget, all jobs
 * together; the pending small files take half, and the rest covers the writers' transient dups,
 * the journals, the pack logs and the sockets. */
static uint32_t g_lf_open, g_lf_peak;

uint32_t ava1_lf_share(void) {
    uint32_t s = ava1_fd_budget() / 4;
    return s < 8 ? 8 : s;
}

uint32_t ava1_lf_job_share(void) {
    uint32_t s = ava1_lf_share() / 2;
    return s < 4 ? 4 : s;
}

int ava1_lf_try_reserve(uint32_t n) {
    uint32_t cur = __atomic_load_n(&g_lf_open, __ATOMIC_SEQ_CST);
    while (cur + n <= ava1_lf_share()) {
        if (__atomic_compare_exchange_n(&g_lf_open, &cur, cur + n, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
            uint32_t pk = __atomic_load_n(&g_lf_peak, __ATOMIC_SEQ_CST);
            while (cur + n > pk &&
                   !__atomic_compare_exchange_n(&g_lf_peak, &pk, cur + n, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {}
            return 1;
        }
    }
    return 0;
}

/* A reserve that cannot refuse (a commit's reopen): counted, so the peak shows an overshoot. */
void ava1_lf_force_reserve(uint32_t n) {
    uint32_t now = __atomic_add_fetch(&g_lf_open, n, __ATOMIC_SEQ_CST);
    uint32_t pk = __atomic_load_n(&g_lf_peak, __ATOMIC_SEQ_CST);
    while (now > pk && !__atomic_compare_exchange_n(&g_lf_peak, &pk, now, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {}
}

void ava1_lf_release(uint32_t n) {
    uint32_t cur = __atomic_load_n(&g_lf_open, __ATOMIC_SEQ_CST);
    while (n) {
        uint32_t take = cur < n ? cur : n;
        if (__atomic_compare_exchange_n(&g_lf_open, &cur, cur - take, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) break;
    }
}

uint32_t ava1_lf_peak(void) { return __atomic_load_n(&g_lf_peak, __ATOMIC_SEQ_CST); }
void ava1_lf_peak_reset(void) { __atomic_store_n(&g_lf_peak, __atomic_load_n(&g_lf_open, __ATOMIC_SEQ_CST), __ATOMIC_SEQ_CST); }

/* Pending-fd reservations held right now (tests: a failed batch must give every one back). */
uint32_t ava1_pend_in_use(void) { return __atomic_load_n(&g_pend_open, __ATOMIC_SEQ_CST); }

uint32_t ava1_pend_peak(void) { return __atomic_load_n(&g_pend_peak, __ATOMIC_SEQ_CST); }
void ava1_pend_peak_reset(void) { __atomic_store_n(&g_pend_peak, __atomic_load_n(&g_pend_open, __ATOMIC_SEQ_CST), __ATOMIC_SEQ_CST); }

void ava1_rpc_msg(uint8_t *out, size_t cap, size_t *out_len, const char *fmt, ...) {
    va_list ap;
    int n;
    *out_len = 0;
    if (!cap) return;
    va_start(ap, fmt);
    n = vsnprintf((char *)out, cap, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    *out_len = (size_t)n >= cap ? cap - 1 : (size_t)n;
}

int ava1_rpc_text(uint8_t *out, size_t cap, size_t *out_len, const char *fmt, ...) {
    static const char cause[] = "reply truncated";
    va_list ap;
    int n;
    *out_len = 0;
    if (!cap) return AVA1_ERR_INTERNAL;
    va_start(ap, fmt);
    n = vsnprintf((char *)out, cap, fmt, ap);
    va_end(ap);
    if (n < 0 || (size_t)n >= cap) {
        size_t c = sizeof cause - 1 < cap ? sizeof cause - 1 : cap;
        memcpy(out, cause, c);
        *out_len = c;
        return AVA1_ERR_INTERNAL;
    }
    *out_len = (size_t)n;
    return AVA1_STATUS_OK;
}

const char *ava1_log_small_flag_path = AVA1_LOG_SMALL_OFF_FLAG;
int ava1_data_log_small_flagged(void) { return access(ava1_log_small_flag_path, F_OK) == 0; }
int ava1_data_log_small(void) { return D.cfg.log_small != AVA1_LOG_SMALL_OFF && !ava1_data_log_small_flagged(); }

int ava1_data_start(const ava1_data_cfg_t *cfg) {
    if (D.running) return -EBUSY; /* one housekeeping thread; a second start changes nothing */
    D.cfg = *cfg;
    if (!D.cfg.budget) D.cfg.budget = 96u << 20;
    ava1_frame_pool_set_budget(D.cfg.budget); /* idle pool memory never exceeds the admit budget */
    if (!D.cfg.workers_start) D.cfg.workers_start = 4;
    if (!D.cfg.workers_min) D.cfg.workers_min = 2;
    if (!D.cfg.workers_max) D.cfg.workers_max = 16;
    /* job->workers[] holds 16; min <= start <= max */
    if (D.cfg.workers_max > 16) D.cfg.workers_max = 16;
    if (D.cfg.workers_min > D.cfg.workers_max) D.cfg.workers_min = D.cfg.workers_max;
    if (D.cfg.workers_start > D.cfg.workers_max) D.cfg.workers_start = D.cfg.workers_max;
    if (D.cfg.workers_start < D.cfg.workers_min) D.cfg.workers_start = D.cfg.workers_min;
    if (!D.cfg.cutoff) D.cfg.cutoff = 256u << 10;
    if (!D.cfg.pack_segment) D.cfg.pack_segment = AVA1_PACK_SEGMENT;
    if (!D.cfg.unswept_max) D.cfg.unswept_max = AVA1_UNSWEPT_MAX;
    if (!D.cfg.sweep_age_ms) D.cfg.sweep_age_ms = AVA1_SWEEP_AGE_MS;
    if (!D.cfg.unswept_total) D.cfg.unswept_total = AVA1_UNSWEPT_TOTAL;
    if (!D.cfg.recover_every_ms) D.cfg.recover_every_ms = 10000u;
    if (!D.cfg.recover_max) D.cfg.recover_max = 4u;
    D.budget_free = D.cfg.budget;
    fd_limit_init();
    __atomic_store_n(&g_pend_open, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&ava1_data_test_fd_budget, 0, __ATOMIC_SEQ_CST);
    D.admitted = 0;
    __atomic_store_n(&g_unswept_total, 0, __ATOMIC_RELAXED); /* a new data layer holds no job's log bytes */
    ava1_data_test_open_delay_ms = ava1_data_test_map_delay_ms = ava1_data_test_feed_delay_ms = 0;
    ava1_data_test_open_work_delay_ms = 0;
    ava1_data_test_ack_fail = ava1_data_test_feeder_fail = 0;
    ava1_data_test_reserve_fail = ava1_data_test_lane_alloc_fail = ava1_data_test_fb_force = 0;
    __atomic_store_n(&D.boot_recovered, D.cfg.jobs_dir[0] ? 0 : 1, __ATOMIC_SEQ_CST);
    D.running = 1;
    if (ava1_thread_start(house_main, NULL, &D.house) != 0) {
        D.running = 0;
        return -EAGAIN;
    }
    if (ava1_thread_start(recover_main, NULL, &D.recover) != 0) {
        D.running = 0;
        pthread_join(D.house, NULL);
        return -EAGAIN;
    }
    return 0;
}

static void flush_one(ava1_job_t *j, void *ctx) {
    uint32_t k;
    (void)ctx;
    if (j->jnl.fd >= 0) (void)fsync(j->jnl.fd);
    if (pthread_mutex_trylock(&j->mu) == 0) { /* psegs may be reallocated under it */
        for (k = 0; k < j->npsegs; k++)
            if (j->psegs[k].fd >= 0 && !j->psegs[k].removed) (void)fsync(j->psegs[k].fd);
        pthread_mutex_unlock(&j->mu);
    }
}

void ava1_data_flush_for_exit(void) { ava1_job_foreach(flush_one, NULL); }

void ava1_data_stop(void) {
    if (!D.running) return;
    D.running = 0;
    pthread_mutex_lock(&D.mu); /* JobOpens in progress, last puts: they end on their own */
    while (D.bg) pthread_cond_wait(&D.cv, &D.mu);
    pthread_mutex_unlock(&D.mu);
    pthread_join(D.house, NULL);
    pthread_join(D.recover, NULL);
    ava1_job_free_all();
}

/* ---- messages ---------------------------------------------------------------------- */

/* For hooks (reader threads): queued on the connection, never waiting for the socket. */
static void post_error(const uint8_t sid[16], uint16_t lane, uint16_t code, const char *msg) {
    ava1_error_t e;
    uint8_t b[256];
    ava1_w_t w;
    memset(&e, 0, sizeof e);
    e.code = code;
    e.message = (const uint8_t *)msg;
    e.message_len = (uint16_t)strlen(msg);
    ava1_w_init(&w, b, sizeof b);
    if (ava1_error_encode(&e, &w) == 0) (void)ava1_server_post(sid, lane, AVA1_TYPE_ERROR, 0, 0, b, w.len);
}

static void post_unknown_map(const uint8_t sid[16], const uint8_t job[16]) {
    static const char why[] = "no such job here: open it";
    ava1_job_map_t m;
    uint8_t b[128];
    ava1_w_t w;
    memset(&m, 0, sizeof m);
    memcpy(m.job_id, job, 16);
    m.status = AVA1_ERR_UNKNOWN_JOB;
    m.last = 1;
    m.has_message = 1;
    m.message = (const uint8_t *)why;
    m.message_len = (uint16_t)(sizeof why - 1);
    ava1_w_init(&w, b, sizeof b);
    if (ava1_job_map_encode(&m, &w) == 0) (void)ava1_server_post(sid, 0, AVA1_TYPE_JOB_MAP, 0, 0, b, w.len);
}

/* Received goes out the moment a lane frame is in memory (SPEC.md §12.3), so it cannot
 * wait for a job thread. A burst of tiny frames could otherwise fill the connection's
 * bounded post queue (which closes the connection when full), so once the queue runs low
 * the ack is handed to a short-lived thread that waits for the socket instead — the
 * queue's writer and that thread serialise on the connection's writer lock. */
#define RECV_POST_FLOOR 8 /* free post-queue entries below which a Received waits */
#define RECV_FB_MAX 32    /* such waiting sends in flight, all sessions together */
#define RECV_FB_POLL_MS 10   /* one sleep of a capped spawn's slot wait */
#define RECV_FB_WAIT_MS 1000 /* a capped spawn waits this long for a slot before posting */

typedef struct {
    uint8_t sid[16];
    size_t len;
    uint8_t body[];
} recv_send_t;

static void *recv_send_main(void *arg) {
    recv_send_t *r = arg;
    (void)ava1_server_send(r->sid, 0, AVA1_TYPE_RECEIVED, 0, 0, r->body, r->len);
    pthread_mutex_lock(&D.mu);
    D.fb--;
    pthread_mutex_unlock(&D.mu);
    free(r);
    return NULL;
}

/* The waiting send for one Received, on a spawned thread. 0, or -1 when the spawn failed
 * (the caller posts instead). When every waiting-send slot is busy the caller waits a
 * bounded moment for one to free up: the post queue must not take the overflow, or a few
 * milliseconds of peer slowness fill it and break the connection. The wait is monotonic
 * (a settimeofday jump must not stretch or cut it short) and never touches the wall
 * clock. */
static int recv_send_spawn(const uint8_t sid[16], const uint8_t *body, size_t len) {
    recv_send_t *r;
    r = malloc(sizeof *r + len);
    if (!r) return -1;
    memcpy(r->sid, sid, 16);
    r->len = len;
    memcpy(r->body, body, len);
    pthread_mutex_lock(&D.mu);
    {
        uint64_t end = ava1_mono_ms() + RECV_FB_WAIT_MS;
        while (D.fb >= RECV_FB_MAX && ava1_mono_ms() < end) {
            pthread_mutex_unlock(&D.mu);
            ava1_platform_sleep_ms(RECV_FB_POLL_MS);
            pthread_mutex_lock(&D.mu);
        }
    }
    if (D.fb >= RECV_FB_MAX) {
        /* No slot after the bound: a peer this slow is as good as gone, and the
         * caller's post closes the connection on a full queue. */
        pthread_mutex_unlock(&D.mu);
        free(r);
        return -1;
    }
    D.fb++;
    pthread_mutex_unlock(&D.mu);
    if (ava1_data_spawn(recv_send_main, r) != 0) {
        pthread_mutex_lock(&D.mu);
        D.fb--;
        pthread_mutex_unlock(&D.mu);
        free(r);
        return -1;
    }
    return 0;
}

static void post_received(const uint8_t sid[16], const uint8_t job[16], uint16_t lane, uint32_t seq) {
    ava1_received_t r;
    uint8_t b[64];
    ava1_w_t w;
    memset(&r, 0, sizeof r);
    memcpy(r.job_id, job, 16);
    r.lane = lane;
    r.seq = seq;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_received_encode(&r, &w) == 0) {
        if (ava1_data_test_fb_force || ava1_server_post_room(sid) < RECV_POST_FLOOR) {
            if (recv_send_spawn(sid, b, w.len) == 0) return;
        }
        (void)ava1_server_post(sid, 0, AVA1_TYPE_RECEIVED, 0, 0, b, w.len);
    }
}

/* A Credit for `job` straight to the session (not through the job's emitter: the caller has
 * already counted it, see the Resume case of route). */
static void post_credit(const uint8_t sid[16], const uint8_t job[16], uint64_t n) {
    ava1_credit_t c;
    uint8_t b[64];
    ava1_w_t w;
    memset(&c, 0, sizeof c);
    memcpy(c.job_id, job, 16);
    c.bytes = n;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_credit_encode(&c, &w) == 0) (void)ava1_server_post(sid, 0, AVA1_TYPE_CREDIT, 0, 0, b, w.len);
}

/* JobOpenAck: a refusal carries why. `post` for hooks; otherwise the waiting send. */
static int send_ack(const uint8_t sid[16], const ava1_job_open_ack_t *ack, const char *msg, int post) {
    ava1_job_open_ack_t a = *ack;
    uint8_t b[256];
    ava1_w_t w;
    if (msg && *msg) {
        a.has_message = 1;
        a.message = (const uint8_t *)msg;
        a.message_len = (uint16_t)strnlen(msg, 160);
    }
    ava1_w_init(&w, b, sizeof b);
    if (ava1_job_open_ack_encode(&a, &w) != 0) return AVA1_E_SPACE;
    return post ? ava1_server_post(sid, 0, AVA1_TYPE_JOB_OPEN_ACK, 0, 0, b, w.len)
                : ava1_server_send(sid, 0, AVA1_TYPE_JOB_OPEN_ACK, 0, 0, b, w.len);
}

static void refuse_open(const uint8_t sid[16], const uint8_t job[16], uint16_t status, const char *msg) {
    ava1_job_open_ack_t a;
    memset(&a, 0, sizeof a);
    memcpy(a.job_id, job, 16);
    a.status = status;
    (void)send_ack(sid, &a, msg, 1);
}

/* A network job's emitter (job threads, workers, the feeder, the open thread: never a
 * reader). The destination is read under cmu, so a re-attach switches it safely; the
 * function itself never changes. Credit it hands out is counted before it is sent (the
 * sender may use it at once); the map lets held lane frames go once it is sent. */
static void net_emit(ava1_job_t *j, uint8_t type, uint8_t flags, const uint8_t *body, size_t len) {
    uint8_t sid[16];
    int attached, map_ok = 0;
    if (type == AVA1_TYPE_JOB_MAP) {
        ava1_job_map_t m;
        map_ok = ava1_job_map_decode(body, len, &m) == 0 && m.status == AVA1_STATUS_OK && m.last;
        if (map_ok && ava1_data_test_map_delay_ms) ava1_platform_sleep_ms(ava1_data_test_map_delay_ms);
    }
    pthread_mutex_lock(&j->cmu);
    attached = j->attached;
    memcpy(sid, j->sid, 16);
    if (attached && type == AVA1_TYPE_CREDIT) {
        ava1_credit_t c;
        if (ava1_credit_decode(body, len, &c) == 0) j->w_avail += c.bytes;
    }
    pthread_mutex_unlock(&j->cmu);
    if (!attached || ava1_server_send(sid, 0, type, flags, 0, body, len) != 0 || !map_ok) return;
    pthread_mutex_lock(&j->cmu);
    if (j->attached && memcmp(j->sid, sid, 16) == 0) {
        j->ready = 1;
        pthread_cond_broadcast(&j->ccv);
    }
    pthread_mutex_unlock(&j->cmu);
}

static void emit_credit(ava1_job_t *j, uint64_t n) {
    ava1_credit_t c;
    uint8_t b[64];
    ava1_w_t w;
    memset(&c, 0, sizeof c);
    memcpy(c.job_id, j->id, 16);
    c.bytes = n;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_credit_encode(&c, &w) == 0) ava1_job_emit(j, AVA1_TYPE_CREDIT, 0, b, w.len);
}

/* A failure found off the job thread: recorded like a worker's (a nonzero final_status
 * on an unfinished job) and ended by the job thread, so JobDone has one emitter and never
 * overtakes a Durable it is still sending. */
void ava1_data_fail_soon(ava1_job_t *j, uint16_t status, const char *what) {
    pthread_mutex_lock(&j->mu);
    if (!j->finished && !j->final_status) {
        j->final_status = status;
        snprintf(j->message, sizeof j->message, "%s", what);
    }
    pthread_mutex_unlock(&j->mu);
}

/* ---- the feeder: a receiver job's frames, off the reader threads ------------------ */

static void free_frame(ava1_inframe_t *f) {
    if (f->cap) (void)ava1_frame_free(f->body, f->cap);
    else free(f->body);
    free(f);
}

static void feed_control(ava1_job_t *j, ava1_inframe_t *f) {
    switch (f->type) {
    case AVA1_TYPE_MANIFEST_PAGE: {
        ava1_manifest_page_t p;
        int rc = ava1_manifest_page_decode(f->body, f->len, &p) == 0 ? ava1_recv_page(j, &p) : AVA1_E_PROTO;
        if (rc == AVA1_E_BADPATH) ava1_data_fail_soon(j, AVA1_ERR_PATH, "the manifest has a path that is not allowed");
        else if (rc == AVA1_E_IO) ava1_data_fail_soon(j, AVA1_ERR_INTERNAL, "out of memory for the manifest");
        else if (rc != 0) ava1_data_fail_soon(j, AVA1_ERR_PROTOCOL, "a manifest page does not follow the rules");
        break;
    }
    case AVA1_TYPE_MANIFEST_END: {
        ava1_manifest_end_t e;
        if (ava1_manifest_end_decode(f->body, f->len, &e) == 0) (void)ava1_recv_end(j, &e);
        break;
    }
    case AVA1_TYPE_RESUME: {
        ava1_resume_t r;
        if (ava1_resume_decode(f->body, f->len, &r) == 0) (void)ava1_recv_resume(j, r.manifest_hash);
        break;
    }
    case AVA1_TYPE_FILE_ROOT: {
        ava1_file_root_t r;
        int rc = ava1_file_root_decode(f->body, f->len, &r) == 0 ? ava1_apply_root(j, r.file_id, r.root) : AVA1_E_PROTO;
        if (rc == AVA1_E_PROTO) ava1_data_fail_soon(j, AVA1_ERR_PROTOCOL, "a FileRoot names no file");
        else if (rc != 0) ava1_data_fail_soon(j, AVA1_ERR_INTERNAL, "out of memory");
        break;
    }
    default:
        break;
    }
}

/* One held lane frame to the apply engine (SPEC.md §12.2: the cutoff decides how a file
 * travels; the engine checks the rest). Takes the frame. */
static void feed_lane(ava1_job_t *j, ava1_inframe_t *f) {
    const uint64_t cutoff = D.cfg.cutoff;
    const char *bad = NULL;
    int ok, rc = 0;
    pthread_mutex_lock(&j->mu);
    ok = j->prepared && !j->finished && !j->stopping;
    if (ok && f->type == AVA1_TYPE_CHUNK) {
        ava1_chunk_t c;
        if (ava1_chunk_decode(f->body, f->len, &c) != 0) bad = "a malformed Chunk";
        else if (c.file_id < j->m.n && j->m.e[c.file_id].size < cutoff) bad = "a Chunk for a file below the cutoff";
    } else if (ok) {
        ava1_bundle_t b;
        ava1_r_t it;
        ava1_bundle_record_t r;
        if (ava1_bundle_decode(f->body, f->len, &b) != 0) {
            bad = "a malformed Bundle";
        } else {
            ava1_r_init(&it, b.records, b.records_len);
            while (!bad && ava1_bundle_record_next(&it, &r) == 1)
                if (r.file_id < j->m.n && j->m.e[r.file_id].size >= cutoff) bad = "a BundleRecord for a file at or above the cutoff";
        }
    }
    pthread_mutex_unlock(&j->mu);
    if (!ok) { /* ended: the frame goes; a job being re-prepared gets its sender the bytes back */
        int fin;
        pthread_mutex_lock(&j->mu);
        fin = j->finished || j->stopping;
        pthread_mutex_unlock(&j->mu);
        if (!fin) emit_credit(j, f->len);
        free_frame(f);
        return;
    }
    if (bad) {
        free_frame(f);
        ava1_data_fail_soon(j, AVA1_ERR_PROTOCOL, bad);
        return;
    }
    if (ava1_data_test_reserve_fail || ava1_apply_reserve(j, f->len) != 0) {
        /* Received already told the sender the frame is here. A live job that cannot take
         * it has lost the frame for good (the sender got its ack), so it ends loudly; an
         * ended job's reserve fails by design and the frame just goes. */
        int fin;
        pthread_mutex_lock(&j->mu);
        fin = j->finished || j->stopping;
        pthread_mutex_unlock(&j->mu);
        if (!fin) ava1_data_fail_soon(j, AVA1_ERR_PROTOCOL, "a data frame beyond the granted credit");
        free_frame(f);
        return;
    }
    if (f->type == AVA1_TYPE_CHUNK) {
        ava1_chunk_t c;
        (void)ava1_chunk_decode(f->body, f->len, &c);
        rc = ava1_apply_chunk_pooled(j, f->body, f->len, f->cap, c.file_id, c.offset, c.data, c.data_len);
    } else {
        ava1_bundle_t b;
        (void)ava1_bundle_decode(f->body, f->len, &b);
        rc = ava1_apply_bundle_pooled(j, f->body, f->len, f->cap, &b);
    }
    free(f); /* the body is the engine's now */
    if (rc == AVA1_E_PROTO) ava1_data_fail_soon(j, AVA1_ERR_PROTOCOL, "a data frame does not fit the manifest");
    else if (rc != 0) ava1_data_fail_soon(j, AVA1_ERR_INTERNAL, "out of memory");
}

/* Control frames in order; a FileRoot names a file of the map, so it (and what follows
 * it) waits for the map like lane frames do. Lane frames go once the map is out. */
static void *feed_main(void *arg) {
    ava1_job_t *j = arg;
    pthread_mutex_lock(&j->cmu);
    for (;;) {
        ava1_inframe_t *f = NULL, *held = NULL;
        int over, oom;
        uint16_t over_status;
        uint64_t gen;
        while (!j->feed_stop && !j->in_overflow && !j->in_oom &&
               !(j->held_head && j->ready) &&
               !(j->in_head && (j->ready || j->in_head->type != AVA1_TYPE_FILE_ROOT)))
            pthread_cond_wait(&j->ccv, &j->cmu);
        if (j->feed_stop) break;
        over = j->in_overflow;
        over_status = j->in_over_status;
        j->in_overflow = 0;
        j->in_over_status = 0;
        oom = j->in_oom;
        j->in_oom = 0;
        gen = j->att_gen; /* the session the batch below belongs to */
        if (j->in_head && (j->ready || j->in_head->type != AVA1_TYPE_FILE_ROOT)) {
            f = j->in_head;
            j->in_head = f->next;
            if (!j->in_head) j->in_tail = NULL;
            j->in_bytes -= f->len;
        } else if (j->ready) {
            held = j->held_head;
            j->held_head = j->held_tail = NULL;
        }
        pthread_mutex_unlock(&j->cmu);
        if (f) ava1_ctl_give(f->len); /* no longer queued: the global control cap */
        if (held && ava1_data_test_feed_delay_ms) ava1_platform_sleep_ms(ava1_data_test_feed_delay_ms);
        if (over)
            ava1_data_fail_soon(j, over_status ? over_status : AVA1_ERR_PROTOCOL, "too many control messages are waiting");
        if (oom) ava1_data_fail_soon(j, AVA1_ERR_INTERNAL, "out of memory");
        if (f) {
            feed_control(j, f);
            free_frame(f);
        }
        while (held) {
            ava1_inframe_t *n = held->next;
            int stale;
            pthread_mutex_lock(&j->cmu);
            /* A re-attach may have run while this batch was taken: its frames name the old
             * session's manifest, so they go (their bytes are the sender's again, unless
             * the attach's grant restarted the allowance and already covers them). */
            stale = j->att_gen != gen;
            if (stale && !j->att_granted) j->w_avail += held->len;
            pthread_mutex_unlock(&j->cmu);
            if (stale) free_frame(held);
            else feed_lane(j, held);
            held = n;
        }
        pthread_mutex_lock(&j->cmu);
    }
    pthread_mutex_unlock(&j->cmu);
    return NULL;
}

static void inbox_add(ava1_job_t *j, uint8_t type, const uint8_t *body, size_t len) {
    ava1_inframe_t *f = calloc(1, sizeof *f);
    uint8_t *b = malloc(len ? len : 1);
    if (!f || !b) {
        free(f);
        free(b);
        f = NULL;
    } else {
        memcpy(b, body, len);
        f->type = type;
        f->len = len;
        f->body = b;
    }
    pthread_mutex_lock(&j->cmu);
    j->frames_in++; /* a root or a page is the sender making progress */
    if (!f || j->in_bytes + len > IN_MAX) {
        j->in_overflow = 1; /* its own inbox full (or no memory): the job ends PROTOCOL */
    } else if (ava1_ctl_take(len) != 0) {
        /* The global control cap (all jobs together): refuse with ERR_BUSY. */
        j->in_overflow = 1;
        j->in_over_status = AVA1_ERR_BUSY;
    } else {
        if (j->in_tail) j->in_tail->next = f;
        else j->in_head = f;
        j->in_tail = f;
        j->in_bytes += len;
        f = NULL;
    }
    pthread_cond_broadcast(&j->ccv);
    pthread_mutex_unlock(&j->cmu);
    if (f) free_frame(f);
}

int ava1_job_attach(ava1_job_t *j, const uint8_t sid[16], uint64_t credit) {
    uint16_t lanes[AVA1_MAX_LANES];
    int nl = ava1_server_lanes(sid, lanes), start = 0;
    ava1_inframe_t *drop, *f;
    if (ava1_job_attach_sid(j, sid) != 0) return -1;
    pthread_mutex_lock(&j->cmu);
    j->emit = net_emit;
    /* Frames held for an earlier session name what its map said: they go, and the new
     * session's frames wait for its own map. Their bytes are the sender's again. A grant
     * (JobOpen, not Resume) restarts the allowance: it is applied after the give-backs, so
     * the sender's credit is exactly the grant (SPEC.md §11.5). */
    j->ready = 0;
    j->att_gen++;
    j->att_granted = credit != 0;
    drop = j->held_head;
    j->held_head = j->held_tail = NULL;
    for (f = drop; f; f = f->next) j->w_avail += f->len;
    if (credit) {
        j->w_avail = credit;
        if (credit > j->w_grant) j->w_grant = credit;
    }
    if (!j->on_frame && !j->feeder_started) start = j->feeder_started = 1;
    pthread_mutex_unlock(&j->cmu);
    __atomic_store_n(&j->lanes, (uint8_t)nl, __ATOMIC_RELAXED);
    while (drop) {
        f = drop->next;
        free_frame(drop);
        drop = f;
    }
    if (start && (ava1_data_test_feeder_fail || ava1_thread_start(feed_main, j, &j->feeder) != 0)) {
        pthread_mutex_lock(&j->cmu);
        j->feeder_started = 0;
        pthread_mutex_unlock(&j->cmu);
        /* The caller answers the session with BUSY: the job must not stay attached to a
         * session its sender believes never opened it. */
        ava1_job_park(j);
        return -1;
    }
    return 0;
}

/* ---- JobOpen ----------------------------------------------------------------------- */

static pthread_mutex_t g_open_mu = PTHREAD_MUTEX_INITIALIZER; /* one ava1_recv_open at a time */

/* A JobOpen in progress and the control frames for its job that arrived meanwhile: they
 * are applied after it, in order, by its thread. */
typedef struct opening_t {
    int used;
    int refused; /* a pipelined frame was not queued: open_now refuses the JobOpen */
    uint16_t refuse_status;
    char refuse_msg[64];
    uint8_t id[16], sid[16], peer[32];
    ava1_inframe_t *head, *tail;
    size_t bytes;
} opening_t;

static struct {
    pthread_mutex_t mu;
    opening_t o[AVA1_MAX_JOBS];
} O = { .mu = PTHREAD_MUTEX_INITIALIZER };

typedef struct {
    const uint8_t *id;
    const char *root;
    ava1_job_t *hit[AVA1_MAX_JOBS];
    int n;
} root_q_t;

/* Under the table lock (ava1_job_foreach), so taking a reference is refs++. A job's root
 * is written once, by ava1_recv_open, which runs under g_open_mu like this check. */
static void root_each(ava1_job_t *j, void *ctx) {
    root_q_t *q = ctx;
    if (memcmp(j->id, q->id, 16) != 0 && (j->kind == AVA1_JOB_UPLOAD || j->kind == AVA1_JOB_COPY) &&
        (ava1_copy_paths_overlap(j->root, q->root) ||
         (j->kind == AVA1_JOB_COPY && __atomic_load_n(&j->copy_move, __ATOMIC_ACQUIRE) &&
          ava1_copy_paths_overlap(j->src, q->root)))) {
        j->refs++;
        q->hit[q->n++] = j;
    }
}

/* Another job that has not ended writes to `root` (Task 13 review M6): two jobs must not
 * stage, lock or rename the same destination. Parked jobs count: they can resume. */
static int root_in_use(const uint8_t id[16], const char *root) {
    root_q_t q;
    int i, busy = 0;
    memset(&q, 0, sizeof q);
    q.id = id;
    q.root = root;
    ava1_job_foreach(root_each, &q);
    for (i = 0; i < q.n; i++) {
        pthread_mutex_lock(&q.hit[i]->mu);
        busy |= !q.hit[i]->finished && !q.hit[i]->stopping;
        pthread_mutex_unlock(&q.hit[i]->mu);
        ava1_job_put(q.hit[i]);
    }
    return busy;
}

/* Consumes `o`'s refusal flag under O.mu. The slot cannot be reused while its open runs
 * (the drain that frees it follows open_now), so the flag always belongs to this open.
 * 1 = a pipelined frame was not queued; `status`/`msg` carry why. */
static int opening_refused(opening_t *o, uint16_t *status, char *msg, size_t cap) {
    int refused;
    pthread_mutex_lock(&O.mu);
    refused = o->refused;
    o->refused = 0;
    *status = o->refuse_status;
    o->refuse_status = 0;
    snprintf(msg, cap, "%s", o->refuse_msg);
    o->refuse_msg[0] = 0;
    pthread_mutex_unlock(&O.mu);
    return refused;
}

/* Runs on the open thread: the work behind a JobOpen (stat, mkdir, journal replay, thread
 * starts) never runs on a reader. */
static void open_now(opening_t *o, const uint8_t sid[16], const uint8_t peer[32], const uint8_t *body,
                     size_t len) {
    ava1_job_open_t q;
    ava1_job_open_ack_t ack;
    char root[AVA1_MAX_PATH + 1], msg[160] = "", refuse_msg[64] = "";
    uint16_t refuse_status = 0;
    ava1_job_t *j = NULL;
    if (ava1_data_test_open_delay_ms) ava1_platform_sleep_ms(ava1_data_test_open_delay_ms);
    if (ava1_job_open_decode(body, len, &q) != 0) return; /* checked by the reader */
    /* A frame pipelined behind this open did not fit the global control cap (or could not
     * be queued): the open is refused instead of letting a partial conversation through.
     * Consumed before the work, so a refused open does none of it, and again after it:
     * the flag can also be set while the work runs. */
    if (o && opening_refused(o, &refuse_status, refuse_msg, sizeof refuse_msg)) {
        refuse_open(sid, q.job_id, refuse_status ? refuse_status : AVA1_ERR_BUSY,
                    refuse_msg[0] ? refuse_msg : "the job could not be opened");
        return;
    }
    memset(&ack, 0, sizeof ack);
    memcpy(ack.job_id, q.job_id, 16);
    if (!D.running) {
        ack.status = AVA1_ERR_BUSY;
        snprintf(msg, sizeof msg, "the console is stopping");
    } else if (q.root_len > AVA1_MAX_PATH || memchr(q.root, 0, q.root_len)) {
        ack.status = AVA1_ERR_PATH;
        snprintf(msg, sizeof msg, "the destination is not a valid path");
    } else if (q.kind == AVA1_JOB_DOWNLOAD) {
        int tries;
        memcpy(root, q.root, q.root_len);
        root[q.root_len] = 0;
        /* No g_open_mu (a large walk would serialize every upload open) and no
         * root_in_use (there is no destination lock for a read). */
        for (tries = 0; tries < 2 && !j; tries++) {
            j = ava1_send_open(&q, peer, &ack, msg, sizeof msg);
            if (!j) break;
            pthread_mutex_lock(&j->cmu);
            j->emit = net_emit; /* C3: before it can matter */
            j->emit_ctx = NULL;
            pthread_mutex_unlock(&j->cmu);
            if (ava1_job_attach(j, sid, 0) != 0) { /* credit 0 = not a grant */
                ava1_job_put(j); /* attached nowhere; ava1_job_attach parked it */
                j = NULL;
                ack.status = AVA1_ERR_BUSY;
                snprintf(msg, sizeof msg, "the job could not be attached");
            } else {
                ava1_send_start(j);
            }
        }
    } else if (q.kind == AVA1_JOB_UPLOAD) {
        ava1_recv_spec_t s;
        int tries;
        memcpy(root, q.root, q.root_len);
        root[q.root_len] = 0;
        memset(&s, 0, sizeof s);
        memcpy(s.id, q.job_id, 16);
        memcpy(s.owner, peer, 32);
        s.kind = q.kind;
        s.policy = q.policy;
        s.flags = q.flags;
        s.root = root;
        s.emit = net_emit;
        s.sid = sid;
        if (!__atomic_load_n(&D.boot_recovered, __ATOMIC_SEQ_CST)) {
            char jd[512];
            ava1_job_dir(D.cfg.jobs_dir, q.job_id, jd, sizeof jd);
            if (ava1_dir_has_pack(jd)) { /* its log is the only copy of files not yet durable: recovery goes first */
                ack.status = AVA1_ERR_BUSY;
                snprintf(msg, sizeof msg, "the console is still recovering this job's files; retry");
                goto open_decided;
            }
        }
        pthread_mutex_lock(&g_open_mu);
        if (ava1_data_test_open_work_delay_ms) ava1_platform_sleep_ms(ava1_data_test_open_work_delay_ms);
        if (root_in_use(q.job_id, root)) {
            ack.status = AVA1_ERR_BUSY;
            snprintf(msg, sizeof msg, "another transfer is writing to this destination");
        } else {
            /* Attach right after the open; a job the reaper unlisted in between (it was
             * parked long enough) is opened again, from its journal. */
            for (tries = 0; tries < 2 && !j; tries++) {
                j = ava1_recv_open(&s, &ack, msg, sizeof msg);
                if (j && ava1_job_attach(j, sid, ack.credit) != 0) {
                    ava1_job_put(j); /* attached nowhere (ava1_job_attach parked it) */
                    j = NULL;
                    ack.status = AVA1_ERR_BUSY;
                    snprintf(msg, sizeof msg, "the job could not be attached");
                } else if (!j) {
                    break;
                }
            }
        }
        pthread_mutex_unlock(&g_open_mu);
open_decided:;
    } else {
        ack.status = AVA1_ERR_PROTOCOL;
        snprintf(msg, sizeof msg, "unknown job kind");
    }
    /* A refusal that landed while the work ran ends the open the same way one that landed
     * before it does: the ack is refused and the job just created goes — its sender
     * believes the open failed, so the job must not stay attached to it (its journal
     * stays: a later JobOpen resumes). */
    if (o && opening_refused(o, &refuse_status, refuse_msg, sizeof refuse_msg)) {
        ack.status = refuse_status ? refuse_status : AVA1_ERR_BUSY;
        snprintf(msg, sizeof msg, "%s", refuse_msg[0] ? refuse_msg : "the job could not be opened");
        if (j) {
            ava1_job_free_one(j->id);
            ava1_job_put(j);
            j = NULL;
        }
    }
    if (j) {
        ava1_log_job_event(ack.status == AVA1_STATUS_OK ? "open" : "open-refused", j, ack.status);
    } else {
        char ev[96];
        snprintf(ev, sizeof ev, "open-refused job=%02x%02x%02x%02x kind=%u status=%u", q.job_id[0], q.job_id[1],
                 q.job_id[2], q.job_id[3], (unsigned)q.kind, (unsigned)ack.status);
        ava1_log_event(ev);
    }
    /* Any send failure means the session is gone or breaking: a job left attached to it
     * would never be reached (and never reaped), so park it. */
    if ((ava1_data_test_ack_fail ? ava1_data_test_ack_fail : send_ack(sid, &ack, j ? "" : msg, 0)) != 0 && j)
        ava1_job_park_session(sid); /* the session ended while we opened: on_session_end missed it */
    if (j) ava1_job_put(j);
}

/* ---- the data plane's RPC methods (SPEC.md §13.5) --------------------------------- */

/* Status carries the local job's progress and how it ended (ext: state, current). */
static int encode_status(ava1_job_t *j, uint8_t *out, size_t cap, size_t *out_len) {
    ava1_status_t st;
    ava1_w_t w;
    int rc;
    if (j->kind == AVA1_JOB_OPKIND) return ava1_op_encode_status(j, out, cap, out_len);
    memset(&st, 0, sizeof st);
    pthread_mutex_lock(&j->mu);
    memcpy(st.job_id, j->id, 16);
    st.files_done = j->files_done;
    st.files_total = j->have_manifest ? j->m.files : j->m_in.files;
    st.bytes_received = j->bytes_received;
    st.bytes_durable = j->bytes_durable;
    st.bytes_total = j->have_manifest ? j->m.bytes : j->m_in.bytes;
    st.workers = j->want_workers;
    if (j->unswept_n) {
        st.has_unswept = 1;
        st.unswept = j->unswept_n;
    }
    if (j->sweep_err) { /* the files cannot be made durable: the sender must hear it */
        st.has_code = 1;
        st.code = AVA1_ERR_IO;
    }
    st.has_state = 1;
    st.state = !j->finished || (j->kind == AVA1_JOB_COPY && j->copy_move && !j->copy_delete_done)
                   ? 0 : (j->final_status == AVA1_STATUS_OK ? 1 : 2);
    if (j->kind == AVA1_JOB_COPY && j->copy_move && j->finished && !j->copy_delete_done) {
        static const char deleting[] = "deleting source";
        st.has_current = 1;
        st.current = (const uint8_t *)deleting;
        st.current_len = sizeof deleting - 1;
    } else if (j->message[0]) {
        st.has_current = 1;
        st.current = (const uint8_t *)j->message;
        st.current_len = (uint16_t)strlen(j->message);
    }
    ava1_w_init(&w, out, cap);
    rc = ava1_status_encode(&st, &w);
    pthread_mutex_unlock(&j->mu);
    *out_len = w.len;
    return rc == 0 ? AVA1_STATUS_OK : AVA1_ERR_INTERNAL;
}

int ava1_data_rpc(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap,
                  size_t *out_len) {
    const uint8_t *peer = ava1_server_rpc_peer();
    *out_len = 0;
    switch (method) {
    case AVA1_METHOD_JOB_COPY: {
        ava1_job_copy_t c;
        ava1_mstore_t prepared = { 0 };
        char dest[AVA1_MAX_PATH + 1], src[AVA1_MAX_PATH + 1];
        char msg[160] = "";
        uint16_t st = AVA1_ERR_PATH;
        ava1_job_t *j;
        int rc, prewalked = 0;
        if (ava1_job_copy_decode(body, len, &c) != 0) return AVA1_ERR_PROTOCOL;
        /* The directory walk may take seconds on a large source. Do it before taking
         * g_open_mu, which also serialises upload JobOpen. Listed jobs need no walk. */
        j = ava1_job_find(c.job_id);
        if (j) ava1_job_put(j);
        else {
            rc = ava1_copy_walk(&c, &prepared);
            if (rc != AVA1_STATUS_OK) {
                ava1_mstore_free(&prepared);
                return rc;
            }
            prewalked = 1;
        }
        /* The open lock is what makes the destination check race-free: ava1_recv_open
         * writes j->root under it (open_now, same rule). */
        pthread_mutex_lock(&g_open_mu);
        if (c.dest_len <= AVA1_MAX_PATH) {
            memcpy(dest, c.dest, c.dest_len);
            dest[c.dest_len] = 0;
            /* A move also takes its source out of the tree: no running writer (upload or
             * copy) may be writing into it, nor another move reading from it. */
            if (c.src_len <= AVA1_MAX_PATH) {
                memcpy(src, c.src, c.src_len);
                src[c.src_len] = 0; /* an embedded NUL only shortens it: copy_open refuses the path */
            } else {
                src[0] = 0;
            }
            if (root_in_use(c.job_id, dest) || ((c.flags & AVA1_JF_MOVE) && src[0] && root_in_use(c.job_id, src))) {
                pthread_mutex_unlock(&g_open_mu);
                ava1_mstore_free(&prepared);
                return AVA1_ERR_BUSY;
            }
            j = ava1_copy_open(&c, peer, prewalked ? &prepared : NULL, &st, msg, sizeof msg);
        } else {
            j = NULL;
        }
        pthread_mutex_unlock(&g_open_mu);
        ava1_mstore_free(&prepared);
        if (!j) return st;
        rc = encode_status(j, out, cap, out_len);
        ava1_job_put(j);
        return rc;
    }
    case AVA1_METHOD_JOB_STATUS:
    case AVA1_METHOD_JOB_CANCEL: {
        ava1_job_ref_t r;
        ava1_job_t *j;
        int st = AVA1_STATUS_OK;
        if (ava1_job_ref_decode(body, len, &r) != 0) return AVA1_ERR_PROTOCOL;
        if (!(j = ava1_job_find(r.job_id))) return AVA1_ERR_UNKNOWN_JOB;
        if (memcmp(j->owner, peer, 32) != 0) {
            ava1_job_put(j);
            return AVA1_ERR_UNKNOWN_JOB;
        }
        if (method == AVA1_METHOD_JOB_STATUS) {
            int fin = j->kind == AVA1_JOB_OPKIND && ava1_op_finished_before(j); /* before encoding */
            st = encode_status(j, out, cap, out_len);
            if (st == AVA1_STATUS_OK && j->kind == AVA1_JOB_OPKIND) ava1_op_status_delivered(j, fin);
        } else if (j->kind == AVA1_JOB_OPKIND) ava1_op_cancel(j); /* an operation: stays listed, finished */
        else ava1_recv_cancel(j); /* stops it and unlists it: the journal stays */
        ava1_job_put(j);
        return st;
    }
    case AVA1_METHOD_JOB_RUN:
        return ava1_op_run_rpc(body, len, peer, out, cap, out_len);
    case AVA1_METHOD_JOB_LIST:
        return ava1_op_list_rpc(peer, out, cap, out_len);
    case AVA1_METHOD_DISK_CALIBRATE:
        return ava1_calibrate(body, len, out, cap, out_len);
    default:
        return -1; /* not ours: the embedder's handler runs (ava1_glue.c, Task 21) */
    }
}

/* ---- control frames ---------------------------------------------------------------- */

static void *cancel_main(void *arg) {
    ava1_job_t *j = arg;
    ava1_recv_cancel(j); /* stops it (the journal stays); may join its threads */
    ava1_job_put(j);
    return NULL;
}

static int route(const uint8_t sid[16], const uint8_t peer[32], uint8_t type, const uint8_t *body, size_t len);

static void *open_main(void *arg) {
    opening_t *o = arg;
    uint8_t sid[16], peer[32];
    memcpy(sid, o->sid, 16);
    memcpy(peer, o->peer, 32);
    for (;;) {
        ava1_inframe_t *f;
        pthread_mutex_lock(&O.mu);
        f = o->head;
        if (f) {
            o->head = f->next;
            if (!o->head) o->tail = NULL;
            o->bytes -= f->len;
        } else {
            o->used = 0; /* from here on, frames for this job go to it directly */
        }
        pthread_mutex_unlock(&O.mu);
        if (!f) break;
        if (f->type == AVA1_TYPE_JOB_OPEN) open_now(o, sid, peer, f->body, f->len);
        else (void)route(sid, peer, f->type, f->body, f->len);
        ava1_ctl_give(f->len); /* no longer queued: the global control cap */
        free_frame(f);
    }
    return NULL;
}

/* Caller holds O.mu. 0, -1 = no memory, -2 = past the global control cap (or the queue's
 * own bound). On -2 nothing was charged. */
static int open_append(opening_t *o, uint8_t type, const uint8_t *body, size_t len) {
    ava1_inframe_t *f;
    if (o->bytes + len > OPEN_MAX || ava1_ctl_take(len) != 0) return -2;
    if (!(f = calloc(1, sizeof *f))) {
        ava1_ctl_give(len);
        return -1;
    }
    if (!(f->body = malloc(len ? len : 1))) {
        ava1_ctl_give(len);
        free(f);
        return -1;
    }
    memcpy(f->body, body, len);
    f->type = type;
    f->len = len;
    if (o->tail) o->tail->next = f;
    else o->head = f;
    o->tail = f;
    o->bytes += len;
    return 0;
}

/* ---- control frames ---------------------------------------------------------------- */

/* A control frame for an existing job (any thread; it never waits on the job). */
static int route(const uint8_t sid[16], const uint8_t peer[32], uint8_t type, const uint8_t *body, size_t len) {
    ava1_job_t *j = ava1_job_find(body);
    int rc = 0;
    if (!j) {
        if (type == AVA1_TYPE_RESUME) post_unknown_map(sid, body); /* SPEC.md §11.5: open it */
        return 0;                                                  /* a late frame for a job that is gone */
    }
    if (memcmp(j->owner, peer, 32) != 0) { /* never another device's job */
        if (type == AVA1_TYPE_RESUME) post_unknown_map(sid, body);
        ava1_job_put_nowait(j);
        return 0;
    }
    if (type == AVA1_TYPE_RESUME) ava1_log_job_event("resume", j, AVA1_STATUS_OK);
    if (j->on_frame) { /* a sender job (Task 18): the receiving peer's acks and map */
        /* A Resume re-attaches it: the new session's lanes that came up before the attach
         * never told the job, so its writers start here (as after a JobOpen's attach). */
        if (type == AVA1_TYPE_RESUME && ava1_job_attach(j, sid, 0) == 0) ava1_send_start(j);
        rc = j->on_frame(j, type, body, len);
        ava1_job_put_nowait(j);
        return rc;
    }
    switch (type) {
    case AVA1_TYPE_RESUME:
        /* ava1_job_attach uses ava1_job_attach_sid: the in-hand equivalent of
         * ava1_job_find_attach (the same listed check under the same lock), since this
         * frame already holds the job's reference. */
        {
            /* SPEC.md §11.5: a Resume restarts the window like a JobOpen's ack does — the
             * sender's allowance is exactly what is free now (the grant minus what the job
             * still holds), counted here and sent as a Credit. */
            uint64_t grant;
            pthread_mutex_lock(&j->mu);
            grant = j->credit > j->outstanding ? j->credit - j->outstanding : 0;
            j->credit_back = 0; /* freed while detached is already in the grant */
            pthread_mutex_unlock(&j->mu);
            if (ava1_job_attach(j, sid, grant) != 0) {
                post_unknown_map(sid, body);
            } else {
                if (grant) post_credit(sid, j->id, grant);
                inbox_add(j, type, body, len);
            }
        }
        break;
    case AVA1_TYPE_JOB_CANCEL:
        if (ava1_data_spawn(cancel_main, j) == 0) return 0; /* the thread has our reference */
        break;
    case AVA1_TYPE_MANIFEST_PAGE:
    case AVA1_TYPE_MANIFEST_END:
    case AVA1_TYPE_FILE_ROOT: {
        /* Only the session the job is attached to may feed it: another session's
         * pipelined pages (say, after a refused open for another destination) must not
         * reach this job's manifest. */
        int mine;
        pthread_mutex_lock(&j->cmu);
        mine = j->attached && memcmp(j->sid, sid, 16) == 0;
        pthread_mutex_unlock(&j->cmu);
        if (mine) inbox_add(j, type, body, len);
        break;
    }
    default:
        break; /* nothing else is for a receiver */
    }
    ava1_job_put_nowait(j);
    return 0;
}

/* A malformed frame of the job conversation closes the session (as any malformed frame). */
static int well_formed(uint8_t type, const uint8_t *body, size_t len) {
    union {
        ava1_job_open_t o;
        ava1_manifest_page_t p;
        ava1_manifest_end_t e;
        ava1_resume_t r;
        ava1_file_root_t f;
        ava1_job_cancel_t c;
    } u;
    switch (type) {
    case AVA1_TYPE_JOB_OPEN: return ava1_job_open_decode(body, len, &u.o) == 0;
    case AVA1_TYPE_MANIFEST_PAGE: return ava1_manifest_page_decode(body, len, &u.p) == 0;
    case AVA1_TYPE_MANIFEST_END: return ava1_manifest_end_decode(body, len, &u.e) == 0;
    case AVA1_TYPE_RESUME: return ava1_resume_decode(body, len, &u.r) == 0;
    case AVA1_TYPE_FILE_ROOT: return ava1_file_root_decode(body, len, &u.f) == 0;
    case AVA1_TYPE_JOB_CANCEL: return ava1_job_cancel_decode(body, len, &u.c) == 0;
    default: return 1;
    }
}

/* Reader thread: decode, route, queue. Nothing here waits for a job or a disk. */
static int data_on_control(const uint8_t sid[16], const uint8_t peer[32], uint8_t type, uint8_t flags,
                           const uint8_t *body, size_t len) {
    int i, slot = -1, rc = 0, queued = 0;
    (void)flags;
    if (!D.running) return 0;
    if (len < 16 || !well_formed(type, body, len)) return 1;
    pthread_mutex_lock(&O.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++) {
        opening_t *o = &O.o[i];
        if (o->used && memcmp(o->id, body, 16) == 0 && memcmp(o->sid, sid, 16) == 0) {
            rc = open_append(o, type, body, len);
            if (rc == -2) { /* the global control cap: the open itself will be refused BUSY */
                o->refused = 1;
                o->refuse_status = AVA1_ERR_BUSY;
                snprintf(o->refuse_msg, sizeof o->refuse_msg, "too many control messages are queued");
            } else if (rc == -1) { /* no memory for a pipelined frame: refused INTERNAL */
                o->refused = 1;
                o->refuse_status = AVA1_ERR_INTERNAL;
                snprintf(o->refuse_msg, sizeof o->refuse_msg, "out of memory");
            }
            queued = 1;
            break;
        }
        if (!o->used && slot < 0) slot = i;
    }
    if (!queued && type == AVA1_TYPE_JOB_OPEN) {
        if (slot < 0) {
            pthread_mutex_unlock(&O.mu);
            refuse_open(sid, body, AVA1_ERR_BUSY, "too many jobs are opening");
            return 0;
        }
        memset(&O.o[slot], 0, sizeof O.o[slot]);
        memcpy(O.o[slot].id, body, 16);
        memcpy(O.o[slot].sid, sid, 16);
        memcpy(O.o[slot].peer, peer, 32);
        rc = open_append(&O.o[slot], type, body, len);
        if (rc != 0) {
            pthread_mutex_unlock(&O.mu);
            refuse_open(sid, body, rc == -2 ? AVA1_ERR_BUSY : AVA1_ERR_INTERNAL,
                        rc == -2 ? "too many control messages are queued" : "out of memory");
            return 0;
        }
        O.o[slot].used = 1;
        pthread_mutex_unlock(&O.mu);
        if (ava1_data_spawn(open_main, &O.o[slot]) != 0) {
            size_t give;
            pthread_mutex_lock(&O.mu);
            give = O.o[slot].bytes;
            O.o[slot].bytes = 0;
            while (O.o[slot].head) {
                ava1_inframe_t *f = O.o[slot].head;
                O.o[slot].head = f->next;
                free_frame(f);
            }
            O.o[slot].used = 0;
            pthread_mutex_unlock(&O.mu);
            if (give) ava1_ctl_give(give);
            refuse_open(sid, body, AVA1_ERR_INTERNAL, "cannot start a thread");
        }
        return 0;
    }
    pthread_mutex_unlock(&O.mu);
    if (queued) return 0; /* queued, or the refusal flag is set for the open thread */
    return route(sid, peer, type, body, len);
}

/* ---- lanes ------------------------------------------------------------------------- */

typedef struct {
    const uint8_t *sid;
    uint64_t bound;
    int any;
} bound_t;

static void bound_each(ava1_job_t *j, void *ctx) {
    bound_t *b = ctx;
    uint64_t g;
    if (!j->attached || memcmp(j->sid, b->sid, 16) != 0) return;
    pthread_mutex_lock(&j->cmu);
    g = j->w_grant;
    pthread_mutex_unlock(&j->cmu);
    b->any = 1;
    if (g > b->bound) b->bound = g;
}

/* Before the body is read. The header names no job (the id is inside the sealed body), so
 * the bound here is the largest credit any job of this session was granted; the exact
 * per-job check follows in on_lane, before anything is done with the frame. The global
 * budget bounds what all readers hold at once. */
static int data_admit(const uint8_t sid[16], uint16_t lane, size_t len) {
    bound_t b;
    int ok;
    if (!D.running) return 1;
    memset(&b, 0, sizeof b);
    b.sid = sid;
    ava1_job_foreach(bound_each, &b);
    if (b.any && len > b.bound) {
        post_error(sid, lane, AVA1_ERR_CREDIT, "a frame larger than the credit granted");
        return 1;
    }
    pthread_mutex_lock(&D.mu);
    ok = D.admitted + len <= D.cfg.budget;
    if (ok) D.admitted += len;
    pthread_mutex_unlock(&D.mu);
    if (!ok) post_error(sid, lane, AVA1_ERR_CREDIT, "too much data in flight");
    return ok ? 0 : 1;
}

/* A lane data frame, in memory. Credit is charged to its job, Received goes out at once
 * (SPEC.md §12.3), and the frame waits in the job until its feeder takes it. */
static int data_on_lane(const uint8_t sid[16], uint16_t lane, uint8_t type, uint32_t seq, uint8_t *body,
                        size_t len) {
    ava1_job_t *j;
    ava1_inframe_t *f;
    int take, over = 0;
    pthread_mutex_lock(&D.mu);
    D.admitted -= len;
    pthread_mutex_unlock(&D.mu);
    if (!body) return 0;
    if (len < 16 || (type != AVA1_TYPE_CHUNK && type != AVA1_TYPE_BUNDLE) || !(j = ava1_job_find(body))) {
        (void)ava1_frame_free(body, ava1_frame_cap(len));
        return 0; /* a frame for a job that is gone: its credit died with it */
    }
    f = calloc(1, sizeof *f);
    if (ava1_data_test_lane_alloc_fail) {
        free(f);
        f = NULL;
    }
    pthread_mutex_lock(&j->cmu);
    if (!f) {
        /* No memory for the frame: Received has not gone out (it is posted only after the
         * alloc succeeds), but the job still cannot recover the frame, so it ends (the
         * feeder records the failure: readers never take j->mu). */
        j->in_oom = 1;
        pthread_cond_broadcast(&j->ccv);
        pthread_mutex_unlock(&j->cmu);
        (void)ava1_frame_free(body, ava1_frame_cap(len));
        ava1_job_put_nowait(j);
        return 0;
    }
    take = j->attached && memcmp(j->sid, sid, 16) == 0 && !j->on_frame;
    if (take && len > j->w_avail) {
        over = 1;
        take = 0;
    }
    if (take) {
        j->w_avail -= len;
        j->frames_in++;
    }
    pthread_mutex_unlock(&j->cmu);
    if (!take) {
        free(f);
        (void)ava1_frame_free(body, ava1_frame_cap(len));
        ava1_job_put_nowait(j);
        if (!over) return 0;
        post_error(sid, lane, AVA1_ERR_CREDIT, "the frame exceeds the credit granted");
        return 1; /* SPEC.md §12.4: the lane closes; the session and the job stay */
    }
    post_received(sid, j->id, lane, seq); /* before any disk work */
    f->type = type;
    f->lane = lane;
    f->seq = seq;
    f->len = len;
    f->body = body;
    f->cap = ava1_frame_cap(len);
    pthread_mutex_lock(&j->cmu);
    if (j->held_tail) j->held_tail->next = f;
    else j->held_head = f;
    j->held_tail = f;
    if (j->ready) pthread_cond_broadcast(&j->ccv);
    pthread_mutex_unlock(&j->cmu);
    ava1_job_put_nowait(j);
    return 0;
}

typedef struct {
    const uint8_t *sid;
    uint16_t lane;
    int up;
} lane_ev_t;

/* Under the table lock: counters and the sender's hook only (it marks state, Task 18). */
static void lane_each(ava1_job_t *j, void *ctx) {
    lane_ev_t *e = ctx;
    if (!j->attached || memcmp(j->sid, e->sid, 16) != 0) return;
    if (e->up) __atomic_add_fetch(&j->lanes, 1, __ATOMIC_RELAXED);
    else if (__atomic_load_n(&j->lanes, __ATOMIC_RELAXED)) __atomic_sub_fetch(&j->lanes, 1, __ATOMIC_RELAXED);
    if (j->on_lane_change) j->on_lane_change(j, e->lane, e->up);
}

static void data_on_lane_change(const uint8_t sid[16], uint16_t lane, int up) {
    lane_ev_t e = { sid, lane, up };
    ava1_job_foreach(lane_each, &e);
}

static void data_on_session_end(const uint8_t sid[16]) { ava1_job_park_session(sid); }

static const ava1_data_hooks_t HOOKS = {
    data_on_control, data_admit, data_on_lane, data_on_lane_change, data_on_session_end,
};

const ava1_data_hooks_t *ava1_data_hooks(void) { return &HOOKS; }
