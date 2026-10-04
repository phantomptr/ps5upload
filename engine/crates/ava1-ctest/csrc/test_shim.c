#ifndef _GNU_SOURCE
#define _GNU_SOURCE /* pthread_getattr_np on glibc */
#endif
/* Starts the payload's AVA1 server on the host with a node.info handler (tests only). */
#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_apply.h"
#include "ava1_stop.h"
#include "ava1_copy.h"
#include "ava1_recv.h"
#include "ava1_conn.h"
#include "ava1_data.h"
#include "ava1_frame.h"
#include "ava1_platform.h"

#include "ava1_gen.h"
#include "ava1_job.h"
#include "ava1_journal.h"
#include "ava1_manifest.h"
#include "ava1_ranges.h"
#include "ava1_send.h"
#include "ava1_server.h"
#include "ava1_thread.h"
#include "mgmt_rpc.h"
#include "path_policy.h"
#include "ava1_op.h"
#include "fs_jobs.h"
#include "net_probe.h"
#include "ava1_events.h"

static uint32_t g_pair_requests, g_last_code, g_logs;


static void on_log(const char *msg) {
    (void)msg;
    __atomic_add_fetch(&g_logs, 1, __ATOMIC_SEQ_CST);
}

static void on_pair(const char *name, uint32_t code) {
    (void)name;
    __atomic_add_fetch(&g_pair_requests, 1, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_last_code, code, __ATOMIC_SEQ_CST);
}


/* ---------------------------------------------------------------------------
 * P3 Task 2: the management dispatcher over stub handlers. The real table lives in
 * payload/src/mgmt_table.def and is built into runtime.c (not compiled on the host); the
 * stubs below answer through the same capture sink runtime.c's send_frame feeds.
 */

#define STUB_FRAME_ERROR 3u

/* What runtime.c's send_frame does with fd == -1 under a management call. */
static int stub_send_frame(uint16_t type, const void *body, uint64_t len) {
    if (mgmt_capture_active()) return mgmt_capture_frame(type, body, len);
    return -1;
}

static size_t stack_size_now(void) {
    size_t sz = 0;
#if defined(__APPLE__)
    sz = pthread_get_stacksize_np(pthread_self());
#else
    pthread_attr_t a;
    if (pthread_getattr_np(pthread_self(), &a) == 0) {
        pthread_attr_getstacksize(&a, &sz);
        pthread_attr_destroy(&a);
    }
#endif
    return sz;
}

static __thread unsigned g_stub_marker;
static uint32_t g_stub_enters, g_stub_leaves, g_stub_last_frame;
static char g_stub_last_path[256];
static uint32_t g_stub_apps;
static pthread_mutex_t g_stub_sony = PTHREAD_MUTEX_INITIALIZER;
static int g_stub_sony_in, g_stub_sony_peak;

static void stub_enter(uint16_t legacy_frame) {
    g_stub_marker = legacy_frame;
    __atomic_add_fetch(&g_stub_enters, 1, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_stub_last_frame, legacy_frame, __ATOMIC_SEQ_CST);
}

static void stub_leave(void) {
    g_stub_marker = 0;
    __atomic_add_fetch(&g_stub_leaves, 1, __ATOMIC_SEQ_CST);
}

/* fs.volumes: a fixed list. */
static int stub_volumes(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char body[] = "{\"volumes\":[{\"path\":\"/data\"}]}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(35, body, sizeof body - 1);
}

/* fs.mkdir: the legacy {"path":...} request, the legacy tokens. */
static int stub_mkdir(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    const char *p = strstr(b, "\"path\":\"");
    size_t n = 0;
    (void)st; (void)fd; (void)t; (void)l;
    g_stub_last_path[0] = '\0';
    if (p) {
        size_t i = 8;
        while (p[i] && p[i] != '"' && n + 2 < sizeof g_stub_last_path) {
            if (p[i] == '\\' && p[i + 1]) g_stub_last_path[n++] = p[i++]; /* an escape: keep both bytes */
            g_stub_last_path[n++] = p[i++];
        }
        g_stub_last_path[n] = '\0';
    }
    if (strcmp(g_stub_last_path, "/denied") == 0) return stub_send_frame(STUB_FRAME_ERROR, "fs_mkdir_path_not_allowed", 25);
    if (strcmp(g_stub_last_path, "/fail") == 0) return stub_send_frame(STUB_FRAME_ERROR, "fs_mkdir_failed", 15);
    return stub_send_frame(47, NULL, 0);
}

/* app.launch (a Sony-lock method): holds a stand-in for sony_api_lock while it works, and
 * answers a successful frame carrying {"ok":false} for a title the stub refuses. */
static int stub_launch(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)l;
    pthread_mutex_lock(&g_stub_sony);
    {
        int in = ++g_stub_sony_in;
        if (in > g_stub_sony_peak) g_stub_sony_peak = in;
    }
    usleep(20 * 1000);
    g_stub_sony_in--;
    pthread_mutex_unlock(&g_stub_sony);
    if (strstr(b, "NOPE")) return stub_send_frame(61, "{\"ok\":false,\"err\":\"launch_failed\"}", 34);
    if (!strstr(b, "title_id")) return stub_send_frame(STUB_FRAME_ERROR, "launch_title_id_missing", 23);
    return stub_send_frame(61, NULL, 0);
}

/* app.list: g_stub_apps entries in the legacy {"apps":[...]} shape (the handler's own buffer
 * is 512 KiB, so it may be larger than a reply can be). */
static int stub_app_list(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    size_t cap = 1u << 20, off = 0;
    uint32_t i;
    char *buf = malloc(cap);
    int rc;
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    if (!buf) return -1;
    off += (size_t)snprintf(buf + off, cap - off, "{\"apps\":[");
    for (i = 0; i < g_stub_apps; i++)
        off += (size_t)snprintf(buf + off, cap - off,
                                "%s{\"title_id\":\"PPSA%05u\",\"title_name\":\"Game \\\"%u\\\", [x]\",\"src\":\"/mnt/a/%u\",\"image_backed\":false}",
                                i ? "," : "", i, i, i);
    off += (size_t)snprintf(buf + off, cap - off, "]}");
    rc = stub_send_frame(63, buf, off);
    free(buf);
    return rc;
}

/* proc.process_list: a reply of the requested size ({"n":N} in the request). */
static int stub_big(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    const char *p = strstr(b, "\"n\":");
    size_t n = p ? (size_t)strtoul(p + 4, NULL, 10) : 0;
    char *buf = malloc(n + 1);
    int rc;
    (void)st; (void)fd; (void)t; (void)l;
    if (!buf) return -1;
    memset(buf, 'x', n);
    buf[n] = '\0';
    rc = stub_send_frame(163, buf, n);
    free(buf);
    return rc;
}

/* fs.mount: reports the environment the dispatcher gave this thread. */
static int stub_env(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    char body[160];
    int n = snprintf(body, sizeof body, "{\"stack\":%zu,\"marker\":%u}", stack_size_now(), g_stub_marker);
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(41, body, (uint64_t)n);
}

/* node.status: the legacy handler's JSON (every key, escapes in the kernel string). */
static int stub_status(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char body[] =
        "{\"version\":\"9.9.9\",\"ps5_kernel\":\"FreeBSD \\\"11\\\" test\","
        "\"instance_id\":18446744073709551000,\"runtime_port\":9113,\"shutdown\":0,\"startup_reason\":2,"
        "\"takeover_requested\":0,\"started_at_unix\":1700000000,\"prior_instance\":\"killed_externally\","
        "\"command_count\":5,\"active_transactions\":0,\"last_tx_seq\":0,\"recovered_transactions\":0,"
        "\"ucred_elevated\":true,\"max_transfer_streams\":4,\"fan_threshold\":70,\"fan_reapply_sec\":30}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(21, body, sizeof body - 1);
}

/* fs.unmount: sends two frames, the second an error (the first error wins; a later OK must not hide it). */
static int stub_two_frames(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    stub_send_frame(STUB_FRAME_ERROR, "fs_unmount_failed", 17);
    return stub_send_frame(55, "{\"ok\":true}", 11);
}

/* fs.mount_pkg: returns without sending anything. */
static int stub_silent(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return 0;
}

/* ---- P3 Task 9: the diagnostics stubs. They answer the way runtime.c's handlers do (same
 * bodies, same limits), so the runners in mgmt_rpc.c (mgmt_call_tail, mgmt_call_probe) and the
 * net.reach probe (payload/src/net_probe.c, the real code) run unchanged on the host. ---- */

static int g_diag_inflight, g_diag_peak;

static void diag_enter(void) {
    int n = __atomic_add_fetch(&g_diag_inflight, 1, __ATOMIC_SEQ_CST), p = __atomic_load_n(&g_diag_peak, __ATOMIC_SEQ_CST);
    while (n > p && !__atomic_compare_exchange_n(&g_diag_peak, &p, n, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
    }
}
static void diag_leave(void) { __atomic_sub_fetch(&g_diag_inflight, 1, __ATOMIC_SEQ_CST); }
static void diag_env_enter(uint16_t frame) {
    (void)frame;
    diag_enter();
}
static void diag_env_leave(void) { diag_leave(); }

static uint32_t g_stub_syslog_len = 200000;
static int g_stub_syslog_mode; /* 0 = text, 1 = the sysctl error frame, 2 = empty */

/* A reproducible log: numbered lines of 40 bytes ("line 00000001 .... \n"), `n` bytes in all. */
static char *numbered_log(size_t n) {
    char *b = malloc(n + 1);
    size_t off = 0, i = 0;
    if (!b) return NULL;
    while (off < n) {
        char line[48];
        int w = snprintf(line, sizeof line, "line %08zu ..............................\n", i++);
        size_t take = (size_t)w < n - off ? (size_t)w : n - off;
        memcpy(b + off, line, take);
        off += take;
    }
    b[n] = '\0';
    return b;
}

/* log.klog: {"max_bytes":N}; like handle_klog_read, default 16 KiB, ceiling 64 KiB. */
static int stub_klog(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    const char *p = strstr(b, "\"max_bytes\"");
    size_t n = 16 * 1024;
    char *log;
    int rc;
    (void)st; (void)fd; (void)t; (void)l;
    if (p && (p = strchr(p, ':')) && atoll(p + 1) > 0) n = (size_t)atoll(p + 1);
    if (n > 64 * 1024) n = 64 * 1024;
    log = numbered_log(n);
    if (!log) return -1;
    rc = stub_send_frame(109, log, n);
    free(log);
    return rc;
}

/* log.syslog: kern.msgbuf, up to 1 MiB (the handler's HARD_CAP). */
static int stub_syslog(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    char *log;
    int rc;
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    if (g_stub_syslog_mode == 3) { /* slow: holds its slot (the in-flight bound tests) */
        usleep(60 * 1000);
    }
    if (g_stub_syslog_mode == 1) return stub_send_frame(STUB_FRAME_ERROR, "syslog_tail_sysctl_errno_12", 27);
    if (g_stub_syslog_mode == 2) return stub_send_frame(145, "", 0);
    log = numbered_log(g_stub_syslog_len);
    if (!log) return -1;
    rc = stub_send_frame(145, log, g_stub_syslog_len);
    free(log);
    return rc;
}

static int stub_netif(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char body[] =
        "{\"interfaces\":[{\"name\":\"eth0\",\"mac\":\"aa:bb:cc:dd:ee:ff\",\"ipv4\":\"192.168.1.50\",\"mtu\":1500,\"flags\":65,\"up\":true}],"
        "\"source\":\"getifaddrs\"}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(111, body, sizeof body - 1);
}

/* net.reach: the real probe. */
static int stub_reach(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    char resp[320];
    size_t n = net_probe_reach(b, (size_t)l, resp, sizeof resp);
    (void)st; (void)fd; (void)t;
    return stub_send_frame(149, resp, n);
}

static int stub_speed(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(123, "{\"ok\":true}", 11);
}

static int stub_modules(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char body[] =
        "{\"modules\":[{\"handle\":1,\"name\":\"libkernel.sprx\",\"base\":\"0x800000000\",\"code_size\":4096}]}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return stub_send_frame(115, body, sizeof body - 1);
}

void ava1_test_mgmt_set_syslog(uint32_t len, int mode) {
    g_stub_syslog_len = len;
    g_stub_syslog_mode = mode;
}

/* mgmt_tail_window(): returns clipped; *start is the window's first byte. */
int ava1_test_tail_window(const char *text, size_t len, size_t cap, size_t *start) {
    return mgmt_tail_window(text, len, cap, start);
}

/* The event log (ava1_events.c). */
void ava1_test_events_set(const char *path, uint32_t limit) {
    ava1_events_set_path(path);
    ava1_events_set_limit(limit);
}
void ava1_test_events_log(const char *line) { ava1_log_event(line); }

#define STUB_RUN(name, helper) \
    static int run_##name(const uint8_t *q, uint32_t n, mgmt_ctx_t *cx) { return helper(q, n, cx, name); }
STUB_RUN(stub_volumes, mgmt_call_text)
STUB_RUN(stub_mkdir, mgmt_call_fs_mkdir)
STUB_RUN(stub_launch, mgmt_call_text)
STUB_RUN(stub_app_list, mgmt_call_paged)
STUB_RUN(stub_big, mgmt_call_text)
STUB_RUN(stub_env, mgmt_call_text)
STUB_RUN(stub_two_frames, mgmt_call_text)
STUB_RUN(stub_silent, mgmt_call_text)
STUB_RUN(stub_status, mgmt_call_node_status)
STUB_RUN(stub_klog, mgmt_call_tail)
STUB_RUN(stub_syslog, mgmt_call_tail)
STUB_RUN(stub_netif, mgmt_call_text)
STUB_RUN(stub_reach, mgmt_call_probe)
STUB_RUN(stub_speed, mgmt_call_text)
STUB_RUN(stub_modules, mgmt_call_text)

static const mgmt_entry_t k_stub_table[] = {
    {AVA1_METHOD_NODE_STATUS, 20, 21, 0, run_stub_status},
    {AVA1_METHOD_FS_VOLUMES, 34, 35, 0, run_stub_volumes},
    {AVA1_METHOD_FS_MKDIR, 46, 47, 0, run_stub_mkdir},
    {AVA1_METHOD_APP_LAUNCH, 60, 61, MGMT_SONY, run_stub_launch},
    {AVA1_METHOD_APP_LIST, 62, 63, 0, run_stub_app_list},
    {AVA1_METHOD_PROC_PROCESS_LIST, 162, 163, 0, run_stub_big},
    {AVA1_METHOD_FS_MOUNT, 52, 53, 0, run_stub_env},
    {AVA1_METHOD_FS_UNMOUNT, 54, 55, 0, run_stub_two_frames},
    {AVA1_METHOD_FS_MOUNT_PKG, 124, 125, 0, run_stub_silent},
};

/* P3 Task 9: its own table (installed by ava1_test_mgmt_install_diag), so it can never collide
 * with the methods other tasks add to k_stub_table. */
static const mgmt_entry_t k_diag_table[] = {
    {AVA1_METHOD_LOG_KLOG, 108, 109, 0, run_stub_klog},
    {AVA1_METHOD_LOG_SYSLOG, 144, 145, 0, run_stub_syslog},
    {AVA1_METHOD_NET_INTERFACES, 110, 111, 0, run_stub_netif},
    {AVA1_METHOD_NET_REACH, 148, 149, 0, run_stub_reach},
    {AVA1_METHOD_NET_SPEEDTEST, 122, 123, 0, run_stub_speed},
    {AVA1_METHOD_PROC_MODULES, 114, 115, 0, run_stub_modules},
};

int ava1_test_mgmt_install_diag(void) {
    __atomic_store_n(&g_diag_inflight, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_diag_peak, 0, __ATOMIC_SEQ_CST);
    return mgmt_rpc_install(k_diag_table, sizeof k_diag_table / sizeof k_diag_table[0], NULL, diag_env_enter,
                            diag_env_leave);
}
int ava1_test_mgmt_diag_peak(void) { return __atomic_load_n(&g_diag_peak, __ATOMIC_SEQ_CST); }

int ava1_test_mgmt_install(void) {
    __atomic_store_n(&g_stub_enters, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_stub_leaves, 0, __ATOMIC_SEQ_CST);
    return mgmt_rpc_install(k_stub_table, sizeof k_stub_table / sizeof k_stub_table[0], NULL, stub_enter, stub_leave);
}

void ava1_test_mgmt_uninstall(void) { (void)mgmt_rpc_install(NULL, 0, NULL, NULL, NULL); }
int ava1_test_mgmt_status_for_token(const char *t) { return mgmt_status_for_token(t); }

/* A table with the same method twice: the install must refuse it. */
int ava1_test_mgmt_install_duplicate(void) {
    mgmt_entry_t dup[2];
    dup[0] = k_stub_table[0];
    dup[1] = k_stub_table[0];
    return mgmt_rpc_install(dup, 2, NULL, NULL, NULL);
}

void ava1_test_mgmt_set_apps(uint32_t n) { g_stub_apps = n; }
void ava1_test_mgmt_stats(uint32_t *enters, uint32_t *leaves, uint32_t *last_frame, int *sony_peak) {
    *enters = __atomic_load_n(&g_stub_enters, __ATOMIC_SEQ_CST);
    *leaves = __atomic_load_n(&g_stub_leaves, __ATOMIC_SEQ_CST);
    *last_frame = __atomic_load_n(&g_stub_last_frame, __ATOMIC_SEQ_CST);
    *sony_peak = g_stub_sony_peak;
}
/* The path fs.mkdir's handler received (after the adapter escaped and the stub read it). */
size_t ava1_test_mgmt_last_path(char *out, size_t cap) {
    size_t n = strlen(g_stub_last_path);
    if (n >= cap) n = cap - 1;
    memcpy(out, g_stub_last_path, n);
    out[n] = '\0';
    return n;
}

static int stack_probe(uint8_t *out, size_t cap, size_t *out_len) {
    int n = snprintf((char *)out, cap, "%zu", stack_size_now());
    *out_len = (size_t)n;
    return AVA1_STATUS_OK;
}

static void fire_payload_stop(void *arg) {
    (void)arg;
    (void)ava1_payload_stop(2000, 2000);
}

/* Off by default: only the Task 8 shutdown tests answer node.shutdown here (the shape of runtime.c's
 * handle_node_shutdown); every other test reaches its installed management table. */
static int g_intercept_shutdown;
void ava1_test_intercept_shutdown(int on) { __atomic_store_n(&g_intercept_shutdown, on, __ATOMIC_SEQ_CST); }

static int rpc(uint16_t method, const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap,
               size_t *out_len) {
    ava1_node_info_t ni;
    ava1_w_t w;
    if (method == 0x7701) { /* holds its slot 1.5 s (the in-flight limit test) */
        usleep(1500 * 1000);
        *out_len = 0;
        return AVA1_STATUS_OK;
    }
    if (method == 0x7702) { /* a reply of the requested size; claims it even past `cap` (guard test) */
        uint32_t n = body_len >= 4 ? (uint32_t)body[0] | (uint32_t)body[1] << 8 | (uint32_t)body[2] << 16 : 0;
        memset(out, 0xAB, n < cap ? n : cap);
        *out_len = n;
        return AVA1_STATUS_OK;
    }
    if (method == 0x7703) { /* ava1_rpc_text into a 16-byte window: "%s" of the request body */
        return ava1_rpc_text(out, 16, out_len, "%.*s", (int)body_len, (const char *)body);
    }
    if (method == AVA1_METHOD_NODE_SHUTDOWN && __atomic_load_n(&g_intercept_shutdown, __ATOMIC_SEQ_CST)) {
        if (ava1_shutdown_defer(300, fire_payload_stop, NULL) != 0) return AVA1_ERR_INTERNAL;
        *out_len = 0; /* SPEC: node.shutdown answers an empty body (mgmt_call_empty) */
        return AVA1_STATUS_OK;
    }
    if (method == 19) return stack_probe(out, cap, out_len); /* a 256 KiB-class method (the data plane's number) */
    if (method != AVA1_METHOD_NODE_INFO) return mgmt_rpc_dispatch(method, body, body_len, out, cap, out_len);
    memset(&ni, 0, sizeof ni);
    ni.version = (const uint8_t *)"test";
    ni.version_len = 4;
    ni.platform = (const uint8_t *)"host";
    ni.platform_len = 4;
    ni.name = (const uint8_t *)"C test server";
    ni.name_len = 13;
    ava1_w_init(&w, out, cap);
    if (ava1_node_info_encode(&ni, &w) != 0) return AVA1_ERR_INTERNAL;
    *out_len = w.len;
    return AVA1_STATUS_OK;
}

/* Mirrored by ava1_ctest::ffi::TestOpts. */
typedef struct {
    uint32_t pairing_s;
    uint32_t ping_ms;
    uint32_t dead_ms;
    uint32_t handshake_ms;
    uint32_t min_frame_rate;
    uint32_t max_conns_per_ip;
    uint32_t max_unpaired;
    uint32_t pair_confirm_ms;
    uint32_t notify_every_ms;
    uint32_t launch; /* 1: launch_key only; 2: launch_key and launch_token */
    uint8_t launch_key[32];
    uint8_t launch_token[16];
} ava1_test_opts_t;

size_t ava1_test_sizeof_opts(void) { return sizeof(ava1_test_opts_t); }

int ava1_test_server_start(const uint8_t secret[32], const char *peers_path, const ava1_test_opts_t *o) {
    ava1_server_cfg_t cfg;
    int rc;
    memset(&cfg, 0, sizeof cfg);
    ava1_identity_from_secret(&cfg.identity, secret);
    strncpy(cfg.name, "C test server", sizeof cfg.name - 1);
    strncpy(cfg.peers_path, peers_path, sizeof cfg.peers_path - 1);
    cfg.bind_loopback = 1;
    cfg.ping_every_ms = o->ping_ms;
    cfg.dead_after_ms = o->dead_ms;
    cfg.handshake_ms = o->handshake_ms;
    cfg.min_frame_rate = o->min_frame_rate;
    cfg.pairing_window_s = o->pairing_s;
    cfg.max_conns_per_ip = o->max_conns_per_ip;
    cfg.max_unpaired = o->max_unpaired;
    cfg.pair_confirm_ms = o->pair_confirm_ms;
    cfg.notify_every_ms = o->notify_every_ms;
    if (o->launch == 2) {
        cfg.has_launch = 1;
        memcpy(cfg.launch_key, o->launch_key, 32);
        memcpy(cfg.launch_token, o->launch_token, 16);
    }
    /* Deliberately asks for the data-plane cap with no hooks — a mistaken embedder. The
     * server must mask it off; `a_server_without_hooks_advertises_no_data_plane_cap`
     * pins that. */
    cfg.caps = AVA1_CAP_DATA_PLANE;
    if (mgmt_rpc_installed()) cfg.caps |= AVA1_CAP_MGMT; /* what ava1_glue.c does */
    cfg.on_pair_request = on_pair;
    cfg.log = on_log;
    cfg.rpc = rpc;
    __atomic_store_n(&g_logs, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_pair_requests, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_last_code, 0, __ATOMIC_SEQ_CST);
    rc = ava1_server_start(&cfg);
    return rc != 0 ? rc : (int)ava1_server_port();
}

uint32_t ava1_test_pair_requests(void) { return __atomic_load_n(&g_pair_requests, __ATOMIC_SEQ_CST); }
uint32_t ava1_test_logs(void) { return __atomic_load_n(&g_logs, __ATOMIC_SEQ_CST); }
uint32_t ava1_test_last_pair_code(void) { return __atomic_load_n(&g_last_code, __ATOMIC_SEQ_CST); }

int ava1_test_conn_open_frame(const uint8_t key[32], const uint8_t *frame, size_t len) {
    int sv[2], rc;
    ava1_conn_t c;
    uint8_t type, flags, buf[256];
    uint32_t ch;
    size_t n;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -100;
    if (write(sv[1], frame, len) != (ssize_t)len) {
        close(sv[0]);
        close(sv[1]);
        return -101;
    }
    close(sv[1]);
    ava1_conn_init(&c, sv[0]);
    memcpy(c.recv_key, key, 32);
    c.keyed = 1;
    rc = ava1_conn_recv(&c, &type, &flags, &ch, buf, sizeof buf, &n);
    ava1_conn_destroy(&c);
    close(sv[0]);
    return rc;
}

/* Review 006 #1, C side. Two keyed ends of a socketpair: (1) frames of every kind (plain,
 * ignorable, a ping) keep both counters in lockstep and distinct; (2) at the nonce ceiling
 * the last counter value still seals and opens, the next send is refused (nothing written,
 * counter unspent, connection broken) and a reader at the ceiling refuses to open.
 * 0 = ok, negative = which check failed. */
int ava1_test_conn_nonce_ceiling(const uint8_t key[32]) {
    int sv[2], rc = 0, i;
    ava1_conn_t a, b;
    uint8_t type, flags, buf[64];
    uint32_t ch;
    size_t n;
    char junk;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -100;
    ava1_conn_init(&a, sv[0]);
    ava1_conn_init(&b, sv[1]);
    memcpy(a.send_key, key, 32);
    memcpy(b.recv_key, key, 32);
    a.keyed = b.keyed = 1;
    for (i = 0; i < 12 && rc == 0; i++) {
        if (a.send_ctr != (uint64_t)i || b.recv_ctr != (uint64_t)i) rc = -1;
        else if (i % 3 == 0 && ava1_conn_send(&a, AVA1_TYPE_CHUNK, 1, (const uint8_t *)"same", 4) != 0) rc = -2;
        else if (i % 3 == 1 && ava1_conn_send_flags(&a, AVA1_TYPE_CHUNK, AVA1_FLAG_IGNORABLE, 1,
                                                    (const uint8_t *)"same", 4) != 0) rc = -3;
        else if (i % 3 == 2 && ava1_conn_send(&a, AVA1_TYPE_PING, 0, NULL, 0) != 0) rc = -4;
        else if (ava1_conn_recv(&b, &type, &flags, &ch, buf, sizeof buf, &n) != 0) rc = -5;
        else if (a.send_ctr != (uint64_t)i + 1 || b.recv_ctr != (uint64_t)i + 1) rc = -6;
    }
    if (rc == 0) {
        a.send_ctr = AVA1_NONCE_CEILING - 1;
        b.recv_ctr = AVA1_NONCE_CEILING - 1;
        if (ava1_conn_send(&a, AVA1_TYPE_CHUNK, 1, (const uint8_t *)"last", 4) != 0) rc = -10;
        else if (ava1_conn_recv(&b, &type, &flags, &ch, buf, sizeof buf, &n) != 0) rc = -11;
        else if (a.send_ctr != AVA1_NONCE_CEILING || b.recv_ctr != AVA1_NONCE_CEILING) rc = -12;
        else if (ava1_conn_send(&a, AVA1_TYPE_CHUNK, 1, (const uint8_t *)"more", 4) == 0) rc = -13;
        else if (a.send_ctr != AVA1_NONCE_CEILING) rc = -14; /* no nonce was spent */
        else if (!a.broken) rc = -15;
        else if (ava1_conn_send(&a, AVA1_TYPE_PING, 0, NULL, 0) == 0) rc = -16;
    }
    if (rc == 0) {
        /* Nothing more reached the peer: the socket carries only EOF now. */
        if (recv(sv[1], &junk, 1, MSG_DONTWAIT) > 0) rc = -17;
    }
    ava1_conn_destroy(&a);
    ava1_conn_destroy(&b);
    close(sv[0]);
    close(sv[1]);
    if (rc == 0) {
        /* A reader already at the ceiling refuses the next frame (sealed under a lower
         * counter here, so it would otherwise open). */
        int sv2[2];
        ava1_conn_t s, r;
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv2) != 0) return -100;
        ava1_conn_init(&s, sv2[0]);
        ava1_conn_init(&r, sv2[1]);
        memcpy(s.send_key, key, 32);
        memcpy(r.recv_key, key, 32);
        s.keyed = r.keyed = 1;
        s.send_ctr = AVA1_NONCE_CEILING - 1;
        r.recv_ctr = AVA1_NONCE_CEILING;
        if (ava1_conn_send(&s, AVA1_TYPE_CHUNK, 1, (const uint8_t *)"last", 4) != 0) rc = -20;
        else if (ava1_conn_recv(&r, &type, &flags, &ch, buf, sizeof buf, &n) == 0) rc = -21;
        else if (r.recv_ctr != AVA1_NONCE_CEILING) rc = -22;
        ava1_conn_destroy(&s);
        ava1_conn_destroy(&r);
        close(sv2[0]);
        close(sv2[1]);
    }
    return rc;
}

/* Review 006 #4: the C reader over `wire` (small frames; the pipe is the socketpair's buffer) keyed with
 * `key`, counter 0: how many frames open before the first error, and that error (AVA1_E_CLOSED at a clean
 * end or a cut frame). The wire is written whole, then the write end closes. */
int ava1_test_conn_read_all(const uint8_t key[32], const uint8_t *wire, size_t len, uint32_t *opened) {
    int sv[2], rc;
    ava1_conn_t c;
    uint8_t type, flags, buf[512];
    uint32_t ch;
    size_t n;
    *opened = 0;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -100;
    if (len && write(sv[1], wire, len) != (ssize_t)len) {
        close(sv[0]);
        close(sv[1]);
        return -101;
    }
    close(sv[1]);
    ava1_conn_init(&c, sv[0]);
    memcpy(c.recv_key, key, 32);
    c.keyed = 1;
    while ((rc = ava1_conn_recv(&c, &type, &flags, &ch, buf, sizeof buf, &n)) == 0) (*opened)++;
    ava1_conn_destroy(&c);
    close(sv[0]);
    return rc;
}

/* The generated per-struct records helpers (SPEC.md §3), which nothing else on the host
 * executes: count the items in `blob`, read each with _next, re-append them with _append
 * (which is also what drives ava1_w_len_begin/_len_end). 0 = ok, -1 = bad blob,
 * -2 = append failed, -3 = writer error. `count` and `out` are the results. */
int ava1_test_records_helpers(const uint8_t *blob, uint32_t len, uint8_t *out, size_t cap,
                              size_t *out_len, uint32_t *count) {
    ava1_r_t it;
    ava1_node_info_t item;
    ava1_w_t w;
    int rc;
    *count = 0;
    *out_len = 0;
    if (ava1_node_info_count(blob, len, count) != 0) return -1;
    ava1_w_init(&w, out, cap);
    ava1_r_init(&it, blob, len);
    while ((rc = ava1_node_info_next(&it, &item)) == 1) {
        if (ava1_node_info_append(&w, &item) != 0) return -2;
    }
    if (rc != 0) return rc;
    *out_len = w.len;
    return w.err != 0 ? -3 : 0;
}

/* ---------------------------------------------------------------------------
 * Echo data hooks (data_hooks.rs): acknowledge every Chunk; on a JOB_DOWNLOAD
 * open, push one 1 MiB chunk of 0x3c on the session's first lane. Admit at
 * most 12 MiB per frame.
 */

static int echo_send(const uint8_t sid[16], uint8_t type, int (*enc)(const void *, ava1_w_t *), const void *m) {
    uint8_t out[128];
    ava1_w_t w;
    ava1_w_init(&w, out, sizeof out);
    if (enc(m, &w) != 0) return -1;
    /* A hook runs on a reader thread: post, never a waiting send. */
    return ava1_server_post(sid, 0, type, 0, 0, out, w.len);
}

static int enc_received(const void *m, ava1_w_t *w) { return ava1_received_encode(m, w); }
static int enc_credit(const void *m, ava1_w_t *w) { return ava1_credit_encode(m, w); }
static int enc_ack(const void *m, ava1_w_t *w) { return ava1_job_open_ack_encode(m, w); }

static int echo_admit(const uint8_t sid[16], uint16_t lane, size_t len) {
    (void)sid;
    (void)lane;
    return len > (12u << 20);
}

static int echo_on_lane(const uint8_t sid[16], uint16_t lane, uint8_t type, uint32_t seq, uint8_t *body,
                        size_t len) {
    ava1_chunk_t c;
    int rc = 0;
    if (!body) return 0;
    if (type == AVA1_TYPE_CHUNK && ava1_chunk_decode(body, len, &c) == 0) {
        ava1_received_t r;
        ava1_credit_t cr;
        memset(&r, 0, sizeof r);
        memcpy(r.job_id, c.job_id, 16);
        r.lane = lane;
        r.seq = seq;
        memset(&cr, 0, sizeof cr);
        memcpy(cr.job_id, c.job_id, 16);
        cr.bytes = c.data_len;
        rc = echo_send(sid, AVA1_TYPE_RECEIVED, enc_received, &r) != 0 ||
             echo_send(sid, AVA1_TYPE_CREDIT, enc_credit, &cr) != 0;
    }
    (void)ava1_frame_free(body, ava1_frame_cap(len)); /* the server read it into a pool buffer */
    return rc;
}

typedef struct {
    uint8_t sid[16];
    uint8_t job[16];
} push_arg_t;

/* Called from on_control, a reader thread: the lane write (and its wait for a live
 * lane) happens on a short-lived thread, never on a reader. */
static void *push_chunk_thread(void *arg) {
    push_arg_t *a = arg;
    uint16_t lanes[AVA1_MAX_LANES];
    ava1_chunk_t c;
    ava1_w_t w;
    size_t cap = AVA1_HEADER_LEN + (1u << 20) + 64 + AVA1_TAG_LEN;
    uint8_t *frame, *data;
    int i, nl = 0;
    for (i = 0; i < 400; i++) { /* the lane may still be proving its key: up to 4 s */
        nl = ava1_server_lanes(a->sid, lanes);
        if (nl > 0) break;
        ava1_platform_sleep_ms(10);
    }
    if (nl == 0) {
        free(a);
        return NULL;
    }
    frame = malloc(cap);
    data = malloc(1u << 20);
    if (!frame || !data) {
        free(frame);
        free(data);
        free(a);
        return NULL;
    }
    memset(data, 0x3c, 1u << 20);
    memset(&c, 0, sizeof c);
    memcpy(c.job_id, a->job, 16);
    c.data = data;
    c.data_len = 1u << 20;
    ava1_w_init(&w, frame + AVA1_HEADER_LEN, cap - AVA1_HEADER_LEN - AVA1_TAG_LEN);
    if (ava1_chunk_encode(&c, &w) == 0)
        (void)ava1_server_send_frame(a->sid, lanes[0], AVA1_TYPE_CHUNK, 1, frame, w.len);
    free(frame);
    free(data);
    free(a);
    return NULL;
}

static int push_chunk(const uint8_t sid[16], const uint8_t job[16]) {
    push_arg_t *a = malloc(sizeof *a);
    pthread_t t;
    if (!a) return -1;
    memcpy(a->sid, sid, 16);
    memcpy(a->job, job, 16);
    if (pthread_create(&t, NULL, push_chunk_thread, a) != 0) {
        free(a);
        return -1;
    }
    pthread_detach(t);
    return 0;
}

static int echo_on_control(const uint8_t sid[16], const uint8_t peer[32], uint8_t type, uint8_t flags,
                           const uint8_t *body, size_t len) {
    ava1_job_open_t o;
    ava1_job_open_ack_t a;
    (void)peer;
    (void)flags;
    if (type != AVA1_TYPE_JOB_OPEN || ava1_job_open_decode(body, len, &o) != 0) return 0;
    if (o.kind == AVA1_JOB_DOWNLOAD) return push_chunk(sid, o.job_id) != 0;
    memset(&a, 0, sizeof a);
    memcpy(a.job_id, o.job_id, 16);
    a.credit = 64u << 20;
    return echo_send(sid, AVA1_TYPE_JOB_OPEN_ACK, enc_ack, &a) != 0;
}

static const ava1_data_hooks_t ECHO = {
    echo_on_control, echo_admit, echo_on_lane, NULL, NULL,
};

int ava1_test_server_start_echo(const uint8_t secret[32], const char *peers_path, uint32_t ping_ms,
                                uint32_t dead_ms, uint32_t handshake_ms) {
    ava1_server_cfg_t cfg;
    int rc;
    memset(&cfg, 0, sizeof cfg);
    ava1_identity_from_secret(&cfg.identity, secret);
    strncpy(cfg.name, "C echo server", sizeof cfg.name - 1);
    strncpy(cfg.peers_path, peers_path, sizeof cfg.peers_path - 1);
    cfg.bind_loopback = 1;
    cfg.ping_every_ms = ping_ms;
    cfg.dead_after_ms = dead_ms;
    cfg.handshake_ms = handshake_ms;
    cfg.rpc = rpc;
    cfg.data = &ECHO;
    cfg.caps = AVA1_CAP_DATA_PLANE;
    rc = ava1_server_start(&cfg);
    return rc != 0 ? rc : (int)ava1_server_port();
}

/* The post queue (ava1_conn_post): post while the writer cannot send (wmu held,
 * nothing read), so nothing can be written; then read every frame back whole and in
 * order. A second conn proves the bound: with the writer parked (it pops under qmu,
 * then waits for wmu), the queue itself holds AVA1_Q_ENTRIES frames, one post more
 * returns AVA1_E_BUSY and breaks the connection. 0 = ok, negative = which check. */
int ava1_test_post_queue(const uint8_t key[32]) {
    int sv[2], rc = 0;
    ava1_conn_t c, peer;
    uint8_t body[64], buf[64];
    uint8_t type, flags;
    uint32_t ch;
    size_t n;
    unsigned i, tries;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -100;
    ava1_conn_init(&c, sv[0]);
    ava1_conn_init(&peer, sv[1]);
    memcpy(c.send_key, key, 32);
    memcpy(c.recv_key, key, 32);
    memcpy(peer.recv_key, key, 32);
    c.keyed = 1;
    peer.keyed = 1;
    pthread_mutex_lock(&c.wmu); /* the writer takes one frame, then waits here */
    for (i = 0; i < AVA1_Q_ENTRIES; i++) {
        memset(body, (int)i, sizeof body);
        if (ava1_conn_post(&c, AVA1_TYPE_CHUNK, 0, i, body, sizeof body) != 0) {
            rc = -101;
            goto out;
        }
    }
    pthread_mutex_unlock(&c.wmu);
    /* Everything queued while nothing could be written arrives whole and in order. */
    for (i = 0; i < AVA1_Q_ENTRIES; i++) {
        if (ava1_conn_recv(&peer, &type, &flags, &ch, buf, sizeof buf, &n) != 0) {
            rc = -102;
            goto out;
        }
        if (type != AVA1_TYPE_CHUNK || ch != i || n != sizeof body || buf[0] != (uint8_t)i || buf[63] != (uint8_t)i) {
            rc = -103;
            goto out;
        }
    }
out:
    ava1_conn_destroy(&peer);
    ava1_conn_destroy(&c);
    close(sv[0]);
    close(sv[1]);
    if (rc != 0) return rc;

    /* The bound, on a fresh conn: the writer pops under qmu and only then waits for
     * wmu, so let it take one frame first — parked, it cannot free room again, and
     * the queue itself is what one post more than AVA1_Q_ENTRIES must exceed. */
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -104;
    ava1_conn_init(&c, sv[0]);
    memcpy(c.send_key, key, 32);
    memcpy(c.recv_key, key, 32);
    c.keyed = 1;
    pthread_mutex_lock(&c.wmu);
    memset(body, 0, sizeof body);
    if (ava1_conn_post(&c, AVA1_TYPE_CHUNK, 0, 0, body, sizeof body) != 0) {
        rc = -105;
        goto bound_out;
    }
    /* The queue's writer pops under qmu and only then waits for wmu, so wait (bounded, in
     * case the thread is starved) until it has taken that first frame: parked on wmu, it
     * cannot free more room. */
    for (tries = 0; tries < 5000; tries++) {
        pthread_mutex_lock(&c.qmu);
        n = c.q_n;
        pthread_mutex_unlock(&c.qmu);
        if (!n) break;
        ava1_platform_sleep_ms(1);
    }
    if (n) {
        rc = -109; /* the writer never took the frame */
        goto bound_out;
    }
    for (i = 0; i < AVA1_Q_ENTRIES; i++) {
        memset(body, (int)i, sizeof body);
        if (ava1_conn_post(&c, AVA1_TYPE_CHUNK, 0, i + 1, body, sizeof body) != 0) {
            rc = -105;
            goto bound_out;
        }
    }
    if (ava1_conn_post(&c, AVA1_TYPE_CHUNK, 0, 0xffffffffu, body, sizeof body) != AVA1_E_BUSY) rc = -106;
    if (rc == 0 && !c.broken) rc = -107; /* a full queue breaks the connection */
    if (rc == 0 && ava1_conn_post(&c, AVA1_TYPE_CHUNK, 0, 0xffffffffu, body, sizeof body) != AVA1_E_IO)
        rc = -108; /* and a broken one refuses everything */
bound_out:
    pthread_mutex_unlock(&c.wmu);
    ava1_conn_destroy(&c);
    close(sv[0]);
    close(sv[1]);
    return rc;
}

/* Applies ops[2i]..ops[2i+1] and writes the resulting pairs to out; returns pair count. */
size_t ava1_test_rset_after(const uint64_t *ops, size_t nops, uint64_t *out, size_t cap_pairs) {
    ava1_rset_t r;
    size_t i, n;
    memset(&r, 0, sizeof r);
    for (i = 0; i < nops; i++) (void)ava1_rset_add(&r, ops[2 * i], ops[2 * i + 1]);
    n = r.n < cap_pairs ? r.n : cap_pairs;
    memcpy(out, r.v, n * 2 * sizeof *out);
    ava1_rset_clear(&r);
    return n;
}

/* FileRun pairs (first, count) for the given set bits; returns pair count. */
size_t ava1_test_bits_runs(uint32_t n, const uint32_t *set, size_t nset, uint32_t *out, size_t cap_pairs) {
    ava1_bits_t b;
    ava1_w_t w;
    ava1_r_t it;
    ava1_file_run_t r;
    uint8_t *blob = malloc(16 + 16 * (size_t)n);
    size_t i, k = 0;
    if (!blob || ava1_bits_init(&b, n) != 0) {
        free(blob);
        return 0;
    }
    for (i = 0; i < nset; i++) ava1_bits_set(&b, set[i]);
    ava1_w_init(&w, blob, 16 + 16 * (size_t)n);
    (void)ava1_bits_append_runs(&b, &w);
    ava1_r_init(&it, blob, w.len);
    while (k < cap_pairs && ava1_file_run_next(&it, &r) == 1) {
        out[2 * k] = r.first;
        out[2 * k + 1] = r.count;
        k++;
    }
    ava1_bits_free(&b);
    free(blob);
    return k;
}

/* Builds the same text the Rust test's c_style_dump builds, from a C replay. */
typedef struct {
    char *out;
    size_t cap, len;
    uint8_t done[4096];
    ava1_rset_t ranges[2048];
    uint8_t roots[2048][32];
    uint8_t has_root[2048];
    int finished;
    uint16_t status;
} jdump_t;

static void jdump_say(jdump_t *d, const char *s) {
    size_t n = strlen(s);
    if (d->len + n < d->cap) {
        memcpy(d->out + d->len, s, n);
        d->len += n;
    }
}

/* 0 on success; nonzero if a records list does not decode (treated as torn, like Rust). */
static int jdump_batch(jdump_t *d, const ava1_jnl_batch_t *b) {
    ava1_r_t it;
    ava1_file_run_t r;
    ava1_file_range_t g;
    ava1_root_item_t ri;
    int rc;
    ava1_r_init(&it, b->files, b->files_len);
    while ((rc = ava1_file_run_next(&it, &r)) == 1) {
        uint32_t f;
        for (f = r.first; f < r.first + r.count && f < 4096; f++) {
            d->done[f] = 1;
            if (f < 2048) ava1_rset_clear(&d->ranges[f]);
        }
    }
    if (rc < 0) return 1;
    ava1_r_init(&it, b->ranges, b->ranges_len);
    while ((rc = ava1_file_range_next(&it, &g)) == 1)
        if (g.file_id < 2048) (void)ava1_rset_add(&d->ranges[g.file_id], g.offset, g.offset + g.len);
    if (rc < 0) return 1;
    ava1_r_init(&it, b->roots, b->roots_len);
    while ((rc = ava1_root_item_next(&it, &ri)) == 1)
        if (ri.file_id < 2048) {
            memcpy(d->roots[ri.file_id], ri.root, 32);
            d->has_root[ri.file_id] = 1;
        }
    return rc < 0 ? 1 : 0;
}

static int jdump_visit(void *ctx, uint8_t kind, const uint8_t *body, size_t len) {
    jdump_t *d = ctx;
    if (kind == AVA1_JNL_BATCH) {
        ava1_jnl_batch_t b;
        if (ava1_jnl_batch_decode(body, len, &b) != 0) return 1;
        if (jdump_batch(d, &b) != 0) return 1;
    } else if (kind == AVA1_JNL_RESET) {
        ava1_jnl_reset_t r;
        if (ava1_jnl_reset_decode(body, len, &r) != 0 || r.file_id >= 2048) return 1;
        d->done[r.file_id] = 0;
        ava1_rset_clear(&d->ranges[r.file_id]);
        d->has_root[r.file_id] = 0;
    } else if (kind == AVA1_JNL_DONE) {
        ava1_jnl_done_t x;
        if (ava1_jnl_done_decode(body, len, &x) != 0) return 1;
        d->finished = 1;
        d->status = x.status;
    } else if (kind == AVA1_JNL_SNAPSHOT) {
        /* Test-only visitor: Rust's replay applies a snapshot, but the cross-tests never
         * write one. Accept it so replay does not stop; the real C replay (Task 13) must
         * apply it. */
    } else if (kind != AVA1_JNL_OPEN) {
        return 1; /* unknown kind: torn from here on, same as Rust's Record::decode */
    }
    return 0;
}

/* Test-only: dump_t is calloc'd, which is what the ava1_rset_t zero-init contract requires. */
size_t ava1_test_journal_dump(const char *dir, char *out, size_t cap) {
    jdump_t *d = calloc(1, sizeof *d);
    ava1_jnl_t j;
    char line[96];
    uint32_t f;
    size_t i, n;
    if (!d) return 0;
    d->out = out;
    d->cap = cap;
    if (ava1_jnl_open(&j, dir, jdump_visit, d) == 0) ava1_jnl_close(&j);
    for (f = 0; f < 4096; f++)
        if (d->done[f]) {
            snprintf(line, sizeof line, "done %u\n", f);
            jdump_say(d, line);
        }
    for (f = 0; f < 2048; f++)
        for (i = 0; i < d->ranges[f].n; i++) {
            snprintf(line, sizeof line, "range %u %llu %llu\n", f,
                     (unsigned long long)d->ranges[f].v[2 * i],
                     (unsigned long long)d->ranges[f].v[2 * i + 1]);
            jdump_say(d, line);
        }
    for (f = 0; f < 2048; f++)
        if (d->has_root[f]) {
            snprintf(line, sizeof line, "root %u %02x\n", f, d->roots[f][0]);
            jdump_say(d, line);
        }
    if (d->finished) snprintf(line, sizeof line, "finished=%u", d->status);
    else snprintf(line, sizeof line, "finished=none");
    jdump_say(d, line);
    for (f = 0; f < 2048; f++) ava1_rset_clear(&d->ranges[f]);
    n = d->len;
    free(d);
    return n;
}

/* Test-only: writes a journal a Rust replay can read. Returns 0 or a negative error, so a
 * failed create/append cannot masquerade as an empty journal. */
int ava1_test_journal_write_sample(const char *dir) {
    ava1_jnl_t j;
    ava1_jnl_open_t o;
    uint8_t body[256], blob[64];
    ava1_w_t w, bw;
    uint32_t i;
    int rc;
    memset(&o, 0, sizeof o);
    o.kind = 1;
    o.root = (const uint8_t *)"/data/c";
    o.root_len = 7;
    rc = ava1_jnl_create(&j, dir, &o);
    if (rc != 0) return rc;
    for (i = 0; i < 10; i++) {
        ava1_jnl_batch_t b;
        ava1_file_run_t r;
        memset(&b, 0, sizeof b);
        memset(&r, 0, sizeof r);
        r.first = i;
        r.count = 1;
        ava1_w_init(&bw, blob, sizeof blob);
        rc = ava1_file_run_append(&bw, &r);
        if (rc != 0) break;
        b.files = blob;
        b.files_len = (uint32_t)bw.len;
        ava1_w_init(&w, body, sizeof body);
        rc = ava1_jnl_batch_encode(&b, &w);
        if (rc != 0) break;
        rc = ava1_jnl_append(&j, AVA1_JNL_BATCH, body, w.len);
        if (rc != 0) break;
    }
    if (rc == 0) {
        ava1_jnl_reset_t r;
        r.file_id = 3;
        ava1_w_init(&w, body, sizeof body);
        rc = ava1_jnl_reset_encode(&r, &w);
        if (rc == 0) rc = ava1_jnl_append(&j, AVA1_JNL_RESET, body, w.len);
    }
    if (rc == 0) {
        ava1_jnl_done_t x;
        x.status = 0;
        ava1_w_init(&w, body, sizeof body);
        rc = ava1_jnl_done_encode(&x, &w);
        if (rc == 0) rc = ava1_jnl_append(&j, AVA1_JNL_DONE, body, w.len);
    }
    ava1_jnl_close(&j);
    return rc;
}

/* Test-only: compacts the journal at `dir` with bodies the Rust test built, so the C
 * compaction's byte layout and its Done-preservation are judged by the Rust reader.
 * Returns 0 or a negative error. */
int ava1_test_journal_compact(const char *dir, const uint8_t *open_b, size_t open_n,
                              const uint8_t *snap_b, size_t snap_n, const uint8_t *done_b,
                              size_t done_n) {
    ava1_jnl_t j;
    int rc = ava1_jnl_open(&j, dir, NULL, NULL);
    if (rc != 0) return rc;
    rc = ava1_jnl_compact(&j, open_b, open_n, snap_b, snap_n, done_b, done_n);
    ava1_jnl_close(&j);
    return rc;
}

/* ---------------------------------------------------------------------------
 * The manifest store (ava1_manifest.c): decode the pages a Rust test encoded,
 * or walk a real tree, and report the C store's hash, entry count and bytes.
 */

int ava1_test_mstore_pages(const uint8_t *const *pages, const size_t *lens, size_t n,
                           uint8_t hash[32], uint32_t *count, uint64_t *bytes) {
    ava1_mstore_t m;
    size_t i;
    int rc = 0;
    memset(&m, 0, sizeof m);
    for (i = 0; i < n && rc == 0; i++) {
        ava1_manifest_page_t p;
        rc = ava1_manifest_page_decode(pages[i], lens[i], &p);
        if (rc == 0) rc = ava1_mstore_add_page(&m, &p);
    }
    ava1_mstore_hash(&m, hash);
    *count = m.n;
    *bytes = m.bytes;
    ava1_mstore_free(&m);
    return rc;
}

int ava1_test_mstore_walk(const char *root, uint8_t hash[32], uint32_t *count) {
    ava1_mstore_t m;
    int rc;
    memset(&m, 0, sizeof m);
    rc = ava1_mstore_walk(&m, root);
    ava1_mstore_hash(&m, hash);
    *count = m.n;
    ava1_mstore_free(&m);
    return rc;
}

/* The walk with flags (AVA1_WALK_FOLLOW): the sender's mode (ruling C1). */
int ava1_test_mstore_walk_ex(const char *root, unsigned flags, uint8_t hash[32], uint32_t *count) {
    ava1_mstore_t m;
    int rc;
    memset(&m, 0, sizeof m);
    rc = ava1_mstore_walk_ex(&m, root, flags);
    ava1_mstore_hash(&m, hash);
    *count = m.n;
    ava1_mstore_free(&m);
    return rc;
}

/* ---------------------------------------------------------------------------
 * ava1_thread_start: the mandated 256 KiB stacks (SPEC.md §15).
 */

static void *smoke(void *arg) {
    volatile uint8_t big[200 * 1024]; /* more than the 64 KiB default-alike rules allow */
    size_t sz = 0;
#if defined(__APPLE__)
    /* macOS pads the reported allocation (measured +12..20 KiB over the request);
     * the smoke's ceiling allows that slack below, and the default 512 KiB stack
     * still fails it. */
    sz = pthread_get_stacksize_np(pthread_self());
#else
    {
        pthread_attr_t a;
        if (pthread_getattr_np(pthread_self(), &a) == 0) {
            pthread_attr_getstacksize(&a, &sz);
            pthread_attr_destroy(&a);
        }
    }
#endif
    big[0] = 1;
    big[sizeof big - 1] = 2;
    *(size_t *)arg = sz;
    return (void *)(uintptr_t)(big[0] + big[sizeof big - 1] == 3 ? 0 : 1);
}

/* 0 only when the 200 KiB frame survived AND the observed stack is within
 * [200 KiB, AVA1_THREAD_STACK + 4 KiB] — 64 KiB of ceiling on macOS, whose
 * pthreads pad the reported allocation — the default pthread stack must not pass. */
int ava1_test_thread_smoke(size_t *stack_bytes) {
    pthread_t t;
    void *ret = (void *)1;
    size_t sz = 0, max = AVA1_THREAD_STACK + 4096u;
#if defined(__APPLE__)
    max = AVA1_THREAD_STACK + 64u * 1024u;
#endif
    if (ava1_thread_start(smoke, &sz, &t) != 0) return -2;
    pthread_join(t, &ret);
    *stack_bytes = sz;
    if ((uintptr_t)ret != 0) return -1;
    return (sz >= 200u * 1024u && sz <= max) ? 0 : -3;
}

/* ---------------------------------------------------------------------------
 * Task 11 fix round: manifest store limits, blob/page cross checks, data config.
 */

size_t ava1_test_ment_size(void) { return sizeof(ava1_ment_t); }

int ava1_test_path_ok(const uint8_t *p, size_t n) { return ava1_path_ok(p, n); }

/* A store holding one file "pre" of `existing` bytes, then one add with the given fields.
 * out[0] = add rc, out[1] = stored size of the new entry (0 if refused), out[2] = store bytes,
 * out[3] = 1 when ava1_mstore_path(id n) is NULL and (id 0) is not. */
void ava1_test_add_one(uint32_t file_id, uint8_t kind, uint64_t size, const uint8_t *path, uint16_t plen,
                       uint64_t existing, uint64_t out[4]) {
    ava1_mstore_t m;
    ava1_manifest_entry_t w;
    memset(&m, 0, sizeof m);
    memset(&w, 0, sizeof w);
    w.path = (const uint8_t *)"pre";
    w.path_len = 3;
    w.kind = AVA1_ENTRY_FILE;
    w.size = existing;
    (void)ava1_mstore_add(&m, &w);
    memset(&w, 0, sizeof w);
    w.file_id = file_id;
    w.kind = kind;
    w.size = size;
    w.path = path;
    w.path_len = plen;
    out[0] = (uint64_t)(int64_t)ava1_mstore_add(&m, &w);
    out[1] = m.n == 2 ? m.e[1].size : 0;
    out[2] = m.bytes;
    out[3] = (ava1_mstore_path(&m, m.n) == NULL && ava1_mstore_path(&m, 0) != NULL) ? 1 : 0;
    ava1_mstore_free(&m);
}

/* out[0] = reserve(MAX+1) rc, out[1] = add rc on a store already holding MAX entries,
 * out[2] = capacity after reserve(223000). */
void ava1_test_mstore_cap(int64_t out[3]) {
    ava1_mstore_t m;
    ava1_manifest_entry_t w;
    memset(&m, 0, sizeof m);
    out[0] = ava1_mstore_reserve(&m, AVA1_MAX_ENTRIES + 1);
    m.n = AVA1_MAX_ENTRIES; /* refused before the table is touched */
    memset(&w, 0, sizeof w);
    w.file_id = AVA1_MAX_ENTRIES;
    w.path = (const uint8_t *)"x";
    w.path_len = 1;
    out[1] = ava1_mstore_add(&m, &w);
    m.n = 0;
    out[2] = ava1_mstore_reserve(&m, 223000) == 0 ? (int64_t)m.cap : -1;
    ava1_mstore_free(&m);
}

/* Pages (Rust-encoded, roots included) -> store -> blob -> second store -> blob again.
 * Returns 0 when the blobs match; hash is the store hash, `blob` the first blob, and the
 * C-encoded pages go to cpages (each u32le length then bytes). */
int ava1_test_mstore_roundtrip(const uint8_t *const *pages, const size_t *lens, size_t n, uint8_t hash[32],
                               uint8_t hash2[32], uint8_t *blob, size_t blob_cap, size_t *blob_len,
                               uint32_t *nroots, uint8_t *cpages, size_t cpages_cap, size_t *cpages_len) {
    ava1_mstore_t m, m2;
    size_t i, off = 0;
    uint8_t *b = NULL, *b2 = NULL;
    size_t bl = 0, bl2 = 0;
    uint32_t next = 0;
    uint8_t job[16] = { 5 };
    int rc = 0;
    memset(&m, 0, sizeof m);
    memset(&m2, 0, sizeof m2);
    for (i = 0; i < n && rc == 0; i++) {
        ava1_manifest_page_t p;
        rc = ava1_manifest_page_decode(pages[i], lens[i], &p);
        if (rc == 0) rc = ava1_mstore_add_page(&m, &p);
    }
    if (rc == 0) rc = ava1_mstore_blob(&m, &b, &bl);
    if (rc == 0) rc = ava1_mstore_from_blob(&m2, b, bl);
    if (rc == 0) rc = ava1_mstore_blob(&m2, &b2, &bl2);
    if (rc == 0 && (bl != bl2 || memcmp(b, b2, bl) != 0)) rc = -100;
    if (rc == 0 && bl > blob_cap) rc = -101;
    if (rc == 0) {
        memcpy(blob, b, bl);
        *blob_len = bl;
        *nroots = m.nroots;
        ava1_mstore_hash(&m, hash);
        ava1_mstore_hash(&m2, hash2);
    }
    while (rc == 0 && next < m.n) {
        size_t len = 0;
        uint8_t *pg = malloc(AVA1_PAGE_BYTES + 64);
        if (!pg) { rc = -102; break; }
        rc = ava1_mstore_page(&m, job, &next, pg, AVA1_PAGE_BYTES + 64, &len);
        if (rc == 0 && off + 4 + len > cpages_cap) rc = -103;
        if (rc == 0) {
            cpages[off] = (uint8_t)len;
            cpages[off + 1] = (uint8_t)(len >> 8);
            cpages[off + 2] = (uint8_t)(len >> 16);
            cpages[off + 3] = (uint8_t)(len >> 24);
            memcpy(cpages + off + 4, pg, len);
            off += 4 + len;
        }
        free(pg);
    }
    *cpages_len = off;
    free(b);
    free(b2);
    ava1_mstore_free(&m);
    ava1_mstore_free(&m2);
    return rc;
}

/* A too-small `out` must leave *next alone; a fitting one advances it.
 * out[0] = rc small, out[1] = next after small, out[2] = rc big, out[3] = next after big. */
void ava1_test_page_next(int64_t out[4]) {
    ava1_mstore_t m;
    uint32_t i, next = 0;
    uint8_t job[16] = { 1 }, small[8], big[4096];
    size_t len = 0;
    memset(&m, 0, sizeof m);
    for (i = 0; i < 3; i++) {
        ava1_manifest_entry_t w;
        char p[8];
        memset(&w, 0, sizeof w);
        snprintf(p, sizeof p, "f%u", i);
        w.file_id = i;
        w.path = (const uint8_t *)p;
        w.path_len = (uint16_t)strlen(p);
        (void)ava1_mstore_add(&m, &w);
    }
    out[0] = ava1_mstore_page(&m, job, &next, small, sizeof small, &len);
    out[1] = next;
    out[2] = ava1_mstore_page(&m, job, &next, big, sizeof big, &len);
    out[3] = next;
    ava1_mstore_free(&m);
}

/* Start with the given workers, report the effective cfg, then start a second time.
 * out[0..3] = start/min/max, out[3] = second start rc, out[4] = first start rc. */
void ava1_test_data_clamp(uint8_t start, uint8_t min, uint8_t max, int out[5]) {
    ava1_data_cfg_t c;
    memset(&c, 0, sizeof c);
    c.workers_start = start;
    c.workers_min = min;
    c.workers_max = max;
    out[4] = ava1_data_start(&c);
    out[0] = ava1_data_cfg()->workers_start;
    out[1] = ava1_data_cfg()->workers_min;
    out[2] = ava1_data_cfg()->workers_max;
    out[3] = ava1_data_start(&c);
    ava1_data_stop();
}

/* ---- the apply engine on a hand-built job (Task 12) ------------------------------ */
/* One apply job at a time: the Rust side (CApplyJob) holds the shared C server lock. */

static pthread_mutex_t g_ev_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_ev[1 << 16];
static size_t g_ev_len;
static int g_ev_done; /* the recorder has seen JOB_DONE */
static int g_same_device = 1;
static ava1_job_t *g_job;
static uint8_t TEST_JOB[16] = { 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7 };
static const uint8_t TEST_OWNER[32] = { 1 };
static int g_trace;           /* record hook points as events */
static uint8_t *g_dup;        /* a chunk to apply again at its file's commit */
static size_t g_dup_len;
static uint32_t g_dup_id;
static uint64_t g_dup_off;

static void ev_add(const char *s, int done);

static int g_arm_point = -1, g_arm_n, g_arm_errno;
/* Probe counters (perf-apply): what the hooks saw, read by ava1_test_apply_probe. */
static unsigned g_pre_calls, g_pre_held, g_commit_calls, g_commit_on_job_thread;
/* Directory syncs (hook 7 = a batch's, hook 10 = prepare's): calls, calls made on a worker, and the
 * distinct threads that made them; `g_hook_sleep_ms` makes each one slow so striping shows. */
static unsigned g_dir_calls[2], g_dir_on_worker[2], g_dir_nthreads[2], g_hook_sleep_ms;
static pthread_t g_dir_threads[2][32];
static pthread_mutex_t g_dir_mu = PTHREAD_MUTEX_INITIALIZER;

static void ava1_test_apply_probe_reset(void) {
    pthread_mutex_lock(&g_dir_mu);
    memset(g_dir_calls, 0, sizeof g_dir_calls);
    memset(g_dir_on_worker, 0, sizeof g_dir_on_worker);
    memset(g_dir_nthreads, 0, sizeof g_dir_nthreads);
    pthread_mutex_unlock(&g_dir_mu);
    g_hook_sleep_ms = 0;
    g_pre_calls = g_pre_held = g_commit_calls = g_commit_on_job_thread = 0;
}

static void dir_hook_note(ava1_job_t *j, int which) {
    pthread_t me = pthread_self();
    unsigned k;
    int on_worker = 0;
    pthread_mutex_lock(&g_dir_mu);
    g_dir_calls[which]++;
    for (k = 0; k < j->nworkers; k++)
        if (pthread_equal(me, j->workers[k])) on_worker = 1;
    g_dir_on_worker[which] += on_worker;
    for (k = 0; k < g_dir_nthreads[which]; k++)
        if (pthread_equal(me, g_dir_threads[which][k])) break;
    if (k == g_dir_nthreads[which] && k < 32) g_dir_threads[which][g_dir_nthreads[which]++] = me;
    pthread_mutex_unlock(&g_dir_mu);
    if (g_hook_sleep_ms) ava1_platform_sleep_ms(g_hook_sleep_ms);
}
typedef struct {
    ava1_job_t *j;
    int acquired, refs;
} pre_watch_t;

static void *pre_watcher(void *arg) {
    pre_watch_t *w = arg;
    pthread_mutex_lock(&w->j->mu);
    __atomic_store_n(&w->acquired, 1, __ATOMIC_SEQ_CST);
    pthread_mutex_unlock(&w->j->mu);
    if (__atomic_sub_fetch(&w->refs, 1, __ATOMIC_SEQ_CST) == 0) free(w);
    return NULL;
}

/* Durable-by-log options for the next job (ava1_test_apply_opts); 0 = default: off, or on when the
 * environment sets AVA1_TEST_LOG_SMALL (the suite is run both ways). The sweep age defaults to 100 ms. */
static uint32_t g_opt_mode, g_opt_seg, g_opt_age, g_opt_every, g_opt_rmax;
static uint64_t g_opt_max, g_opt_total;
/* v = mode, pack_segment, unswept_max, sweep_age_ms, unswept_total, recover_every_ms, recover_max, job id byte (0 = 7) */
void ava1_test_apply_opts2(const uint64_t v[8]) {
    g_opt_mode = (uint32_t)v[0];
    g_opt_seg = (uint32_t)v[1];
    g_opt_max = v[2];
    g_opt_age = (uint32_t)v[3];
    g_opt_total = v[4];
    g_opt_every = (uint32_t)v[5];
    g_opt_rmax = (uint32_t)v[6];
    memset(TEST_JOB, v[7] ? (int)v[7] : 7, 16);
}
static void apply_opts(ava1_data_cfg_t *cfg) {
    cfg->log_small = (uint8_t)(g_opt_mode ? g_opt_mode : (getenv("AVA1_TEST_LOG_SMALL") ? AVA1_LOG_SMALL_ON : AVA1_LOG_SMALL_OFF));
    cfg->pack_segment = g_opt_seg;
    cfg->unswept_max = g_opt_max;
    cfg->sweep_age_ms = g_opt_age ? g_opt_age : 100;
    cfg->unswept_total = g_opt_total;
    cfg->recover_every_ms = g_opt_every;
    cfg->recover_max = g_opt_rmax;
}
/* review 007 #5: point the console's durable-by-log off-switch at a test file (NULL: the real path). */
static char g_log_flag_buf[1024];
void ava1_test_set_log_small_flag(const char *path) {
    if (!path) {
        ava1_log_small_flag_path = AVA1_LOG_SMALL_OFF_FLAG;
        return;
    }
    snprintf(g_log_flag_buf, sizeof g_log_flag_buf, "%s", path);
    ava1_log_small_flag_path = g_log_flag_buf;
}
static int g_hold_commit;                           /* commits wait at COMMIT_VERIFIED while set */
static uint32_t g_prealloc_fault = UINT32_MAX - 1;  /* a file whose preallocation answers ENOSPC */
static void t_hook(ava1_job_t *j, int point, uint32_t id) {
    if (point == __atomic_load_n(&g_arm_point, __ATOMIC_SEQ_CST)) { /* fsync fault, armed for this point */
        ava1_fsync_test_errno = g_arm_errno;
        __atomic_store_n(&ava1_fsync_test_fail_n, g_arm_n, __ATOMIC_SEQ_CST);
        __atomic_store_n(&g_arm_point, -1, __ATOMIC_SEQ_CST);
    }
    if (point == AVA1_HOOK_PREALLOC) {
        /* Called right before the preallocation. A watcher thread takes j->mu while this one
         * waits 40 ms: if the mutex is held by this thread the watcher cannot get it in that
         * time (another worker holding it briefly cannot take 40 ms). */
        pre_watch_t *w = calloc(1, sizeof *w);
        pthread_t th;
        __atomic_add_fetch(&g_pre_calls, 1, __ATOMIC_SEQ_CST);
        if (w) {
            w->j = j;
            w->refs = 2;
            if (pthread_create(&th, NULL, pre_watcher, w) == 0) {
                pthread_detach(th);
                ava1_platform_sleep_ms(40);
                if (!__atomic_load_n(&w->acquired, __ATOMIC_SEQ_CST))
                    __atomic_add_fetch(&g_pre_held, 1, __ATOMIC_SEQ_CST);
                if (__atomic_sub_fetch(&w->refs, 1, __ATOMIC_SEQ_CST) == 0) free(w);
            } else {
                free(w);
            }
        }
    }
    if (point == AVA1_HOOK_BATCH_DIR_SYNCED) dir_hook_note(j, 0);
    if (point == AVA1_HOOK_PREP_DIR_SYNCED) dir_hook_note(j, 1);
    if (point == AVA1_HOOK_COMMIT_VERIFIED) {
        __atomic_add_fetch(&g_commit_calls, 1, __ATOMIC_SEQ_CST);
        if (pthread_equal(pthread_self(), j->thread)) __atomic_add_fetch(&g_commit_on_job_thread, 1, __ATOMIC_SEQ_CST);
        while (__atomic_load_n(&g_hold_commit, __ATOMIC_SEQ_CST) && !__atomic_load_n(&j->stopping, __ATOMIC_SEQ_CST))
            ava1_platform_sleep_ms(2);
    }
    if (__atomic_load_n(&g_trace, __ATOMIC_SEQ_CST)) {
        char line[64];
        snprintf(line, sizeof line, "hook %d %u\n", point, id);
        ev_add(line, 0);
    }
    if (point == AVA1_HOOK_COMMIT_VERIFIED && g_dup && id == g_dup_id) {
        uint8_t *own = g_dup;
        size_t n = g_dup_len;
        g_dup = NULL;
        if (ava1_apply_reserve(j, n) != 0) {
            free(own);
            return;
        }
        (void)ava1_apply_chunk(j, own, n, id, g_dup_off, own, n);
        /* wait until a worker has applied it (the commit itself runs on a worker: it is the one busy) */
        pthread_mutex_lock(&j->mu);
        while (j->q_len || j->busy > 1) {
            pthread_mutex_unlock(&j->mu);
            ava1_platform_sleep_ms(1);
            pthread_mutex_lock(&j->mu);
        }
        pthread_mutex_unlock(&j->mu);
    }
}

/* Fails the next `n` fsync tries with `err`: now (point < 0) or the next time the apply engine reaches the
 * hook `point` (so a test can aim at the journal's fsync, which follows hook 7). */
void ava1_test_fsync_fault(int point, int n, int err) {
    if (point < 0) {
        ava1_fsync_test_errno = err;
        __atomic_store_n(&ava1_fsync_test_fail_n, n, __ATOMIC_SEQ_CST);
    } else {
        g_arm_n = n;
        g_arm_errno = err;
        __atomic_store_n(&g_arm_point, point, __ATOMIC_SEQ_CST);
    }
}
unsigned ava1_test_fsync_calls(void) { return __atomic_load_n(&ava1_fsync_calls_total, __ATOMIC_RELAXED); }
unsigned ava1_test_fsync_retries(void) { return __atomic_load_n(&ava1_fsync_retries_total, __ATOMIC_RELAXED); }
int ava1_test_fsync_pending_faults(void) { return __atomic_load_n(&ava1_fsync_test_fail_n, __ATOMIC_SEQ_CST); }

/* out[0] = preallocations seen, out[1] = of those, how many ran with the job mutex held,
 * out[2] = commits begun, out[3] = of those, how many ran on the job thread. */
void ava1_test_apply_probe(uint64_t out[8]) {
    memset(out, 0, 8 * sizeof out[0]);
    out[0] = __atomic_load_n(&g_pre_calls, __ATOMIC_SEQ_CST);
    out[1] = __atomic_load_n(&g_pre_held, __ATOMIC_SEQ_CST);
    out[2] = __atomic_load_n(&g_commit_calls, __ATOMIC_SEQ_CST);
    out[3] = __atomic_load_n(&g_commit_on_job_thread, __ATOMIC_SEQ_CST);
    pthread_mutex_lock(&g_dir_mu);
    out[4] = g_dir_calls[0];
    out[5] = g_dir_on_worker[0];
    out[6] = g_dir_nthreads[0];
    out[7] = g_job ? (uint64_t)__atomic_load_n(&g_job->perchunk, __ATOMIC_SEQ_CST) : 0;
    pthread_mutex_unlock(&g_dir_mu);
}

size_t ava1_test_apply_summary(char *out, size_t cap) { return g_job ? ava1_apply_summary(g_job, out, cap) : 0; }
int ava1_test_apply_timing(void) { return g_job ? g_job->timing : 0; }

/* The same for prepare's directory syncs: out[0..3] = calls, on a worker, distinct threads. */
void ava1_test_apply_probe_prep(uint64_t out[3]) {
    pthread_mutex_lock(&g_dir_mu);
    out[0] = g_dir_calls[1];
    out[1] = g_dir_on_worker[1];
    out[2] = g_dir_nthreads[1];
    pthread_mutex_unlock(&g_dir_mu);
}

/* Each directory sync the apply engine reports (hooks 7 and 10) then takes `ms` more. */
void ava1_test_apply_hook_sleep(uint32_t ms) { __atomic_store_n(&g_hook_sleep_ms, ms, __ATOMIC_SEQ_CST); }

unsigned ava1_test_house_ticks(void) { return __atomic_load_n(&ava1_house_ticks, __ATOMIC_RELAXED); }
uint64_t ava1_test_unswept_total(void) { return ava1_unswept_total(); }
void ava1_test_unswept_global_add(int64_t d) { ava1_unswept_add(d); }
uint64_t ava1_test_apply_unswept_bytes(void) {
    uint64_t n;
    if (!g_job) return 0;
    pthread_mutex_lock(&g_job->mu);
    n = g_job->unswept_bytes;
    pthread_mutex_unlock(&g_job->mu);
    return n;
}
/* ava1_jobs_gc over `jobs_dir` as if `age_s` seconds had passed and the limit were `max_age_s`. */
int ava1_test_jobs_gc(const char *jobs_dir, int64_t age_s, int64_t max_age_s) {
    return ava1_jobs_gc(jobs_dir, (int64_t)time(NULL) + age_s, max_age_s);
}
uint32_t ava1_test_apply_unswept(void) {
    uint32_t n;
    if (!g_job) return 0;
    pthread_mutex_lock(&g_job->mu);
    n = g_job->unswept_n;
    pthread_mutex_unlock(&g_job->mu);
    return n;
}
/* How many pack.* files the job directory holds right now. */
uint32_t ava1_test_apply_segments(void) {
    uint32_t n = 0;
    DIR *dp;
    struct dirent *de;
    if (!g_job || !(dp = opendir(g_job->dir))) return 0;
    while ((de = readdir(dp)) != NULL)
        if (strncmp(de->d_name, "pack.", 5) == 0) n++;
    closedir(dp);
    return n;
}
void ava1_test_apply_hold_commit(int on) { __atomic_store_n(&g_hold_commit, on, __ATOMIC_SEQ_CST); }
void ava1_test_apply_fault_prealloc(uint32_t id) { __atomic_store_n(&g_prealloc_fault, id, __ATOMIC_SEQ_CST); }
/* ava1_apply_compact's answer now (0 compacted, -1 skipped). */
int ava1_test_apply_compact(void) { return ava1_apply_compact(g_job); }
/* Commits queued or running. */
uint32_t ava1_test_apply_commits_inflight(void) {
    uint32_t n;
    pthread_mutex_lock(&g_job->mu);
    n = g_job->commits_inflight;
    pthread_mutex_unlock(&g_job->mu);
    return n;
}

void ava1_test_apply_trace(int on) { __atomic_store_n(&g_trace, on, __ATOMIC_SEQ_CST); }

static uint32_t g_fault_id = UINT32_MAX - 1; /* no file: no fault */
static int g_sweep_fail_n; /* sweeps' file syncs fail with EIO this many times (-1: until cleared) */
int ava1_test_sweep_fail_left(void) { return __atomic_load_n(&g_sweep_fail_n, __ATOMIC_SEQ_CST); }
void ava1_test_sweep_fail(int n) { __atomic_store_n(&g_sweep_fail_n, n, __ATOMIC_SEQ_CST); }
static int t_fault(ava1_job_t *j, int point, uint32_t id) {
    (void)j;
    if (point == AVA1_HOOK_SWEEP_FILE) {
        int n = __atomic_load_n(&g_sweep_fail_n, __ATOMIC_SEQ_CST);
        while (n != 0) {
            if (n < 0) return EIO;
            if (__atomic_compare_exchange_n(&g_sweep_fail_n, &n, n - 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) return EIO;
        }
        return 0;
    }
    if (point == AVA1_HOOK_PREALLOC && id == __atomic_load_n(&g_prealloc_fault, __ATOMIC_SEQ_CST)) return ENOSPC;
    return point == AVA1_HOOK_DIR_SYNCED && id == __atomic_load_n(&g_fault_id, __ATOMIC_SEQ_CST) ? EIO : 0;
}
void ava1_test_apply_fail_dir_sync(uint32_t id) { __atomic_store_n(&g_fault_id, id, __ATOMIC_SEQ_CST); }

void ava1_test_apply_hold(int on) { __atomic_store_n(&ava1_apply_hold_batches, on, __ATOMIC_SEQ_CST); }

uint32_t ava1_test_apply_pending(void) {
    uint32_t n;
    pthread_mutex_lock(&g_job->mu);
    n = g_job->pend_n;
    pthread_mutex_unlock(&g_job->mu);
    return n;
}

int ava1_test_apply_dup_on_commit(uint32_t id, uint64_t off, const uint8_t *d, size_t len) {
    uint8_t *own = malloc(len ? len : 1);
    if (!own) return -1;
    memcpy(own, d, len);
    g_dup_id = id;
    g_dup_off = off;
    g_dup_len = len;
    g_dup = own; /* set before any chunk of the file is sent: no race with the hook */
    return 0;
}

/* Read by the data layer's same_device hook from the job's threads. */
void ava1_test_set_same_device(int v) { __atomic_store_n(&g_same_device, v, __ATOMIC_SEQ_CST); }
static int t_same_device(const char *a, const char *b) {
    int v = __atomic_load_n(&g_same_device, __ATOMIC_SEQ_CST);
    if (v == -2) return -1; /* (not reached with a NULL hook) */
    if (v == 2) { /* "a mount": the path is on another device than any different folder */
        char ra[1024], rb[1024];
        if (!realpath(a, ra) || !realpath(b, rb)) return -1;
        return strcmp(ra, rb) == 0 ? 1 : 0;
    }
    return v;
}
static int g_deny_write;
static uint8_t g_kind = AVA1_JOB_UPLOAD, g_owner = 1; /* the receiver driver's JobOpen */
static int t_allow(const char *p) {
    if (strstr(p, "/ps5-denied/")) return 0; /* the path-policy tests (job.run delete/chmod) */
    return !__atomic_load_n(&g_deny_write, __ATOMIC_SEQ_CST);
}
/* The data layer's may_read hook (downloads): a test flips it through the FFI setter. */
static int g_allow_read = 1;
void ava1_test_set_allow_read(int v) { __atomic_store_n(&g_allow_read, v, __ATOMIC_SEQ_CST); }
/* Review S2: the data layer's refuse_link hook, wired to the real shared policy when a test names a protected
 * directory (NULL turns it off). */
static int g_protect_links;
void ava1_test_set_protected(const char *dir) {
    path_policy_set_protected(dir);
    __atomic_store_n(&g_protect_links, dir != NULL, __ATOMIC_SEQ_CST);
}
static int t_refuse_link(const char *p) { return __atomic_load_n(&g_protect_links, __ATOMIC_SEQ_CST) && path_tree_op_refused(p); }
static int t_allow_read(const char *p, int u) {
    (void)p;
    (void)u;
    return __atomic_load_n(&g_allow_read, __ATOMIC_SEQ_CST);
}

static void ev_add(const char *s, int done) {
    size_t n = strlen(s);
    pthread_mutex_lock(&g_ev_mu);
    if (g_ev_len + n < sizeof g_ev) {
        memcpy(g_ev + g_ev_len, s, n);
        g_ev_len += n;
    }
    if (done) g_ev_done = 1;
    pthread_mutex_unlock(&g_ev_mu);
}

static void rec_emit(ava1_job_t *j, uint8_t type, uint8_t flags, const uint8_t *body, size_t len) {
    char line[256];
    (void)j;
    (void)flags;
    if (type == AVA1_TYPE_DURABLE) {
        ava1_durable_t d;
        ava1_r_t it;
        ava1_file_run_t r;
        size_t at;
        if (ava1_durable_decode(body, len, &d) != 0) return;
        snprintf(line, sizeof line, "durable files=");
        at = strlen(line);
        ava1_r_init(&it, d.files, d.files_len);
        while (ava1_file_run_next(&it, &r) == 1 && at < sizeof line - 24)
            at += (size_t)snprintf(line + at, sizeof line - at, "%u+%u,", r.first, r.count);
        snprintf(line + at, sizeof line - at, " ranges=%u\n", d.ranges_count);
        ev_add(line, 0);
    } else if (type == AVA1_TYPE_FILE_RETRY) {
        ava1_file_retry_t r;
        if (ava1_file_retry_decode(body, len, &r) != 0) return;
        snprintf(line, sizeof line, "retry %u %u\n", r.file_id, r.reason);
        ev_add(line, 0);
    } else if (type == AVA1_TYPE_JOB_DONE) {
        ava1_job_done_t d;
        if (ava1_job_done_decode(body, len, &d) != 0) return;
        /* one ev_add: a waiter released by the done flag also sees the message */
        if (d.has_message)
            snprintf(line, sizeof line, "%sdone %u\nmsg %.*s\n", d.has_settling && d.settling ? "settling\n" : "", d.status,
                     (int)d.message_len, (const char *)d.message);
        else
            snprintf(line, sizeof line, "%sdone %u\n", d.has_settling && d.settling ? "settling\n" : "", d.status);
        ev_add(line, 1);
    } else if (type == AVA1_TYPE_STATUS) {
        ava1_status_t st;
        static uint16_t last_code;
        if (ava1_status_decode(body, len, &st) != 0) return;
        if (st.has_code && st.code && st.code != last_code) { /* a receiver-side failure the sender must see */
            snprintf(line, sizeof line, "status code=%u unswept=%u\n", st.code, st.has_unswept ? st.unswept : 0);
            ev_add(line, 0);
        }
        last_code = st.has_code ? st.code : 0;
    } else if (type == AVA1_TYPE_JOB_MAP) {
        ava1_job_map_t m;
        ava1_r_t it;
        ava1_file_run_t r;
        size_t at;
        if (ava1_job_map_decode(body, len, &m) != 0) return;
        at = (size_t)snprintf(line, sizeof line, "map status=%u last=%u done=", m.status, m.last);
        ava1_r_init(&it, m.done, m.done_len);
        while (ava1_file_run_next(&it, &r) == 1 && at < sizeof line - 40)
            at += (size_t)snprintf(line + at, sizeof line - at, "%u+%u,", r.first, r.count);
        snprintf(line + at, sizeof line - at, " partial=%u\n", m.partial_count);
        ev_add(line, 0);
    }
}

static void mkdir_p(const char *p) {
    char b[1200];
    size_t i;
    snprintf(b, sizeof b, "%s", p);
    for (i = 1; b[i]; i++)
        if (b[i] == '/') {
            b[i] = 0;
            (void)mkdir(b, 0755);
            b[i] = '/';
        }
    (void)mkdir(b, 0755);
}

/* 0, or a negative step; on failure everything begin set up is torn down again. */
int ava1_test_apply_begin(const char *jobs_dir, const char *root, uint32_t flags, const uint8_t *blob, size_t len,
                          uint32_t fsync_delay_us, int crash_at) {
    ava1_data_cfg_t cfg;
    ava1_jnl_open_t o;
    struct stat st;
    uint32_t i;
    int rc = 0;
    ava1_test_set_same_device(1); /* a test that died mid-way must not leak its override */
    __atomic_store_n(&ava1_fsync_test_fail_n, 0, __ATOMIC_SEQ_CST); /* ... or its fsync fault */
    __atomic_store_n(&g_arm_point, -1, __ATOMIC_SEQ_CST);
    ava1_test_apply_probe_reset();
    memset(&cfg, 0, sizeof cfg);
    snprintf(cfg.jobs_dir, sizeof cfg.jobs_dir, "%s", jobs_dir);
    cfg.may_write = t_allow;
    cfg.may_read = t_allow_read;
    cfg.refuse_link = t_refuse_link;
    cfg.same_device = g_same_device == -2 ? NULL : t_same_device; /* -2: no hook at all */
    cfg.fsync_delay_us = fsync_delay_us;
    cfg.crash_at = crash_at;
    apply_opts(&cfg);
    if (ava1_data_start(&cfg) != 0) return -1;
    g_trace = 0;
    ava1_apply_hook = t_hook;
    ava1_apply_fault = t_fault;
    pthread_mutex_lock(&g_ev_mu);
    g_ev_len = 0;
    g_ev_done = 0;
    pthread_mutex_unlock(&g_ev_mu);
    g_job = ava1_job_create(TEST_JOB, TEST_OWNER);
    if (!g_job) {
        ava1_data_stop();
        return -2;
    }
    g_job->kind = AVA1_JOB_UPLOAD;
    g_job->flags = flags;
    snprintf(g_job->root, sizeof g_job->root, "%s", root);
    /* Staging is decided here, once: a root that appears later must not be moved over. */
    g_job->staged = !(flags & AVA1_JF_SINGLE_FILE) && stat(root, &st) != 0;
    snprintf(g_job->base, sizeof g_job->base, "%s%s", root, g_job->staged ? ".ava-part" : "");
    mkdir_p(jobs_dir);
    ava1_job_dir(jobs_dir, TEST_JOB, g_job->dir, sizeof g_job->dir);
    mkdir_p(g_job->dir);
    if (ava1_mstore_from_blob(&g_job->m, blob, len) != 0) {
        rc = -3;
        goto fail;
    }
    ava1_mstore_hash(&g_job->m, g_job->manifest_hash);
    if (ava1_bits_init(&g_job->done, g_job->m.n) != 0) {
        rc = -4;
        goto fail;
    }
    g_job->lf = calloc(g_job->m.n + 1, sizeof *g_job->lf);
    if (!g_job->lf) {
        rc = -4;
        goto fail;
    }
    if (!(flags & AVA1_JF_SINGLE_FILE)) {
        mkdir_p(g_job->base);
        for (i = 0; i < g_job->m.n; i++)
            if (g_job->m.e[i].kind == AVA1_ENTRY_DIR) {
                char p[1200];
                const char *rel = ava1_mstore_path(&g_job->m, i);
                snprintf(p, sizeof p, "%s/%s", g_job->base, rel ? rel : "");
                mkdir_p(p);
            }
    }
    memset(&o, 0, sizeof o);
    memcpy(o.job_id, TEST_JOB, 16);
    memcpy(o.manifest_hash, g_job->manifest_hash, 32);
    o.kind = AVA1_JOB_UPLOAD;
    o.flags = flags;
    o.staged = (uint8_t)g_job->staged;
    o.root = (const uint8_t *)root;
    o.root_len = (uint16_t)strlen(root);
    if (ava1_jnl_create(&g_job->jnl, g_job->dir, &o) != 0) {
        rc = -5;
        goto fail;
    }
    g_job->credit = ava1_budget_take(ava1_data_cfg()->budget, 1); /* the whole budget, taken */
    g_job->emit = rec_emit;
    g_job->prepared = 1;
    if (ava1_apply_start(g_job) != 0) {
        rc = -6;
        goto fail;
    }
    return 0;
fail:
    ava1_job_put(g_job);
    g_job = NULL;
    ava1_data_stop();
    return rc;
}

int ava1_test_apply_chunk(uint32_t id, uint64_t off, const uint8_t *d, size_t len) {
    uint8_t *own = malloc(len ? len : 1);
    if (!own) return -1;
    memcpy(own, d, len);
    if (ava1_apply_reserve(g_job, len) != 0) {
        free(own);
        return -2;
    }
    return ava1_apply_chunk(g_job, own, len, id, off, own, len);
}

int ava1_test_apply_record(uint32_t id, const uint8_t *d, size_t len, const uint8_t root[32]) {
    ava1_bundle_record_t r;
    ava1_bundle_t b;
    size_t cap = len + 128;
    uint8_t *own = malloc(cap);
    ava1_w_t w;
    if (!own) return -1;
    memset(&r, 0, sizeof r);
    r.file_id = id;
    memcpy(r.root, root, 32);
    r.data = d;
    r.data_len = (uint32_t)len;
    ava1_w_init(&w, own, cap);
    if (ava1_bundle_record_append(&w, &r) != 0) {
        free(own);
        return -2;
    }
    memset(&b, 0, sizeof b);
    b.records = own;
    b.records_len = (uint32_t)w.len;
    b.records_count = 1;
    if (ava1_apply_reserve(g_job, cap) != 0) {
        free(own);
        return -3;
    }
    return ava1_apply_bundle(g_job, own, cap, &b);
}

int ava1_test_apply_bundle_raw(const uint8_t *d, size_t len, uint32_t count) {
    ava1_bundle_t b;
    uint8_t *own = malloc(len ? len : 1);
    if (!own) return -1;
    memcpy(own, d, len);
    memset(&b, 0, sizeof b);
    b.records = own;
    b.records_len = (uint32_t)len;
    b.records_count = count;
    if (ava1_apply_reserve(g_job, len) != 0) {
        free(own);
        return -3;
    }
    return ava1_apply_bundle(g_job, own, len, &b);
}

int ava1_test_apply_root(uint32_t id, const uint8_t root[32]) { return ava1_apply_root(g_job, id, root); }

/* The final status, once the job is finished AND its JobDone has been recorded
 * (ava1_apply_fail sets `finished` before it journals Done and emits). -1 on timeout. */
int ava1_test_apply_wait(uint32_t timeout_ms) {
    uint64_t t0 = ava1_mono_ms();
    while (ava1_mono_ms() - t0 < timeout_ms) {
        int fin, st, seen;
        pthread_mutex_lock(&g_job->mu);
        fin = g_job->finished;
        st = g_job->final_status;
        pthread_mutex_unlock(&g_job->mu);
        pthread_mutex_lock(&g_ev_mu);
        seen = g_ev_done;
        pthread_mutex_unlock(&g_ev_mu);
        if (fin && seen) return st;
        ava1_platform_sleep_ms(10);
    }
    return -1;
}

size_t ava1_test_apply_events(char *out, size_t cap) {
    size_t n;
    pthread_mutex_lock(&g_ev_mu);
    n = g_ev_len < cap ? g_ev_len : cap;
    memcpy(out, g_ev, n);
    pthread_mutex_unlock(&g_ev_mu);
    return n;
}

void ava1_test_apply_end(void) {
    {
        static const uint64_t none[8] = { 0 };
        ava1_test_apply_opts2(none);
    }
    __atomic_store_n(&g_sweep_fail_n, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_hold_commit, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_prealloc_fault, UINT32_MAX - 1, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_hook_sleep_ms, 0, __ATOMIC_SEQ_CST);
    if (g_job) ava1_job_put(g_job);
    g_job = NULL;
    ava1_data_stop();
    ava1_apply_hook = NULL;
    ava1_apply_fault = NULL;
    ava1_test_apply_fail_dir_sync(UINT32_MAX - 1);
    ava1_test_apply_hold(0);
    __atomic_store_n(&ava1_fsync_test_fail_n, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_arm_point, -1, __ATOMIC_SEQ_CST);
    g_kind = AVA1_JOB_UPLOAD;
    g_owner = 1;
    __atomic_store_n(&g_deny_write, 0, __ATOMIC_SEQ_CST);
    g_trace = 0;
    free(g_dup);
    g_dup = NULL;
    ava1_test_set_same_device(1);
}

/* ---- the receiver driven directly (Task 13) --------------------------------------- */
/* Shares g_job, the recorder and the apply calls above; one job at a time (the Rust side
 * holds the C server lock). */

static ava1_data_cfg_t g_cfg;
static char g_root[2048]; /* room past AVA1_MAX_PATH: refusals of long roots are tested */
static uint32_t g_flags, g_entries;
static uint8_t g_policy;
static ava1_job_open_ack_t g_ack;
static int g_last_open;

static void ev_reset(void) {
    pthread_mutex_lock(&g_ev_mu);
    g_ev_len = 0;
    g_ev_done = 0;
    pthread_mutex_unlock(&g_ev_mu);
}

static int recv_open_now(void) {
    ava1_recv_spec_t s;
    char msg[160];
    memset(&s, 0, sizeof s);
    memcpy(s.id, TEST_JOB, 16);
    memcpy(s.owner, TEST_OWNER, 32);
    s.owner[0] = g_owner;
    s.kind = g_kind;
    s.policy = g_policy;
    s.flags = g_flags;
    s.entries = g_entries;
    s.root = g_root;
    s.emit = rec_emit;
    g_job = ava1_recv_open(&s, &g_ack, msg, sizeof msg);
    g_last_open = g_job ? 0 : (int)g_ack.status;
    return g_last_open;
}

static int recv_start(int crash_at) {
    ava1_test_apply_probe_reset(); /* the counters cover the run since the last (re)open */
    g_cfg.crash_at = crash_at;
    ev_reset();
    if (ava1_data_start(&g_cfg) != 0) return -100;
    ava1_apply_hook = t_hook;
    ava1_apply_fault = t_fault;
    return recv_open_now();
}

/* 0, or the refusal's status (the job is then not open). */
int ava1_test_recv_open(const char *jobs_dir, const char *root, uint32_t flags, uint8_t policy, uint32_t entries,
                        int crash_at) {
    ava1_test_set_same_device(1);
    memset(&g_cfg, 0, sizeof g_cfg);
    snprintf(g_cfg.jobs_dir, sizeof g_cfg.jobs_dir, "%s", jobs_dir);
    g_cfg.may_write = t_allow;
    g_cfg.may_read = t_allow_read;
    g_cfg.refuse_link = t_refuse_link;
    g_cfg.same_device = t_same_device;
    apply_opts(&g_cfg);
    snprintf(g_root, sizeof g_root, "%s", root);
    g_flags = flags;
    g_policy = policy;
    g_entries = entries;
    g_trace = 0;
    return recv_start(crash_at);
}

uint8_t ava1_test_recv_staged(void) { return g_ack.staged; }

/* A payload restart: every job and thread gone, the disk kept. */
/* A JobOpen for job id byte*16 (root `root`, the kinds/flags of the last open) as a session would send it: 0 if it
 * opened (the job is freed again), else the refusal's status. */
int ava1_test_probe_open(uint8_t byte, const char *root) {
    ava1_recv_spec_t s;
    ava1_job_open_ack_t ack;
    char msg[160];
    ava1_job_t *j;
    memset(&s, 0, sizeof s);
    memset(s.id, byte, 16);
    memcpy(s.owner, TEST_OWNER, 32);
    s.owner[0] = (uint8_t)g_owner;
    s.kind = AVA1_JOB_UPLOAD;
    s.policy = g_policy;
    s.flags = g_flags;
    s.root = root;
    s.emit = rec_emit;
    j = ava1_recv_open(&s, &ack, msg, sizeof msg);
    if (!j) return (int)ack.status;
    ava1_job_free_one(j->id);
    ava1_job_put(j);
    return 0;
}
/* The data layer stops (the job dropped first), and nothing starts it again. */
void ava1_test_data_stop_only(void) {
    if (g_job) ava1_job_put(g_job);
    g_job = NULL;
    ava1_data_stop();
}

/* A helper restart where no JobOpen follows: only the start-time recovery runs. 0, or the start's error. */
int ava1_test_recv_restart_noopen(void) {
    if (g_job) ava1_job_put(g_job);
    g_job = NULL;
    ava1_data_stop();
    g_cfg.crash_at = 0;
    ev_reset();
    return ava1_data_start(&g_cfg);
}

/* The housekeeping reap as if an hour had passed, then the shim's own reference too: a settling job is
 * destroyed with its files unswept (the directory stays for recovery). */
void ava1_test_reap_and_drop(void) {
    ava1_job_reap(ava1_mono_ms() + 3600u * 1000u);
    if (g_job) ava1_job_put(g_job);
    g_job = NULL;
}

int ava1_test_recv_restart(int crash_at) {
    if (g_job) ava1_job_put(g_job);
    g_job = NULL;
    ava1_data_stop();
    return recv_start(crash_at);
}

int ava1_test_recv_page(const uint8_t *page, size_t len) {
    ava1_manifest_page_t p;
    int rc = ava1_manifest_page_decode(page, len, &p);
    return rc ? rc : ava1_recv_page(g_job, &p);
}

int ava1_test_recv_end(uint32_t files, uint64_t bytes, const uint8_t hash[32]) {
    ava1_manifest_end_t e;
    memset(&e, 0, sizeof e);
    memcpy(e.job_id, TEST_JOB, 16);
    e.files = files;
    e.bytes = bytes;
    memcpy(e.manifest_hash, hash, 32);
    return ava1_recv_end(g_job, &e);
}

int ava1_test_recv_resume(const uint8_t hash[32]) { return ava1_recv_resume(g_job, hash); }

/* Holds job.run's reply until the operation it started has finished: the race where an op ends
 * while its "running" reply is being built (it must stay listed for the next job.status). */
static void wait_op_finished(ava1_job_t *j) {
    for (int i = 0; i < 2000 && !__atomic_load_n(&j->finished, __ATOMIC_ACQUIRE); i++) usleep(1000);
}
void ava1_test_op_hold_reply_until_finished(int on) { ava1_op_test_pre_encode = on ? wait_op_finished : NULL; }

int ava1_test_job_stopped(void) {
    int s;
    pthread_mutex_lock(&g_job->mu);
    s = g_job->stopping;
    pthread_mutex_unlock(&g_job->mu);
    return s;
}

int ava1_test_recv_last_open(void) { return g_last_open; }
uint64_t ava1_test_recv_ack_credit(void) { return g_ack.credit; }

/* NULL root / owner 0 / deny < 0: unchanged. */
void ava1_test_recv_set(const char *root, uint8_t owner, int deny) {
    if (root) snprintf(g_root, sizeof g_root, "%s", root);
    if (owner) g_owner = owner;
    if (deny >= 0) __atomic_store_n(&g_deny_write, deny, __ATOMIC_SEQ_CST);
}

void ava1_test_recv_args(uint8_t kind) { g_kind = kind; }

/* Another JobOpen in the same process. drop_old releases the current session's reference
 * first; when the open is refused the current job (if kept) stays current. */
int ava1_test_recv_reopen(int drop_old) {
    ava1_job_t *old = g_job;
    int rc;
    if (drop_old && old) {
        ava1_job_put(old);
        old = NULL;
    }
    g_job = NULL;
    rc = recv_open_now();
    if (old) {
        if (g_job) ava1_job_put(old);
        else g_job = old;
    }
    return rc;
}

int ava1_test_apply_reserve(size_t n, int take) {
    if (!take) {
        ava1_apply_unreserve(g_job, n);
        return 0;
    }
    return ava1_apply_reserve(g_job, n);
}

/* ---- P3 Task 5: job.run operations wrapped around stub FTX2 handlers -------------------- */

/* A slow stub: `loops` rounds of 20 ms, each reporting one unit of progress and honouring
 * job.cancel the way backup.c does (mgmt_op_cancelled / mgmt_op_progress). */
static int stub_slow(const char *b, const char *cancel_token, int cancel_frame_type) {
    uint32_t i, loops = 0;
    const char *p = strstr(b, "\"loops\":");
    if (p) loops = (uint32_t)strtoul(p + 8, NULL, 10);
    mgmt_op_total(loops, (uint64_t)loops * 100u);
    for (i = 0; i < loops; i++) {
        if (mgmt_op_cancelled()) return stub_send_frame((uint16_t)cancel_frame_type, cancel_token, strlen(cancel_token));
        usleep(20 * 1000);
        mgmt_op_progress(1, 100);
    }
    return 0;
}

static int stub_op_fsck(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)l;
    if (strstr(b, "\"error\"")) return stub_send_frame(STUB_FRAME_ERROR, "libSceFsInternalForVsh_unavailable", strlen("libSceFsInternalForVsh_unavailable"));
    if (stub_slow(b, "fsck_cancelled", STUB_FRAME_ERROR) != 0) return 0;
    if (strstr(b, "dirty")) /* a normal frame whose body says ok:false: the operation's answer, not a failure */
        return stub_send_frame(127, "{\"ok\":false,\"code\":3,\"device\":\"/dev/md1\",\"repair\":false}", strlen("{\"ok\":false,\"code\":3,\"device\":\"/dev/md1\",\"repair\":false}"));
    return stub_send_frame(127, "{\"ok\":true,\"code\":0,\"device\":\"/dev/md1\",\"repair\":false}", strlen("{\"ok\":true,\"code\":0,\"device\":\"/dev/md1\",\"repair\":false}"));
}

static int stub_op_snapshot(void *st, int fd, uint64_t t, const char *b) {
    (void)st; (void)fd; (void)t;
    if (strstr(b, "\"fail\"")) return stub_send_frame(STUB_FRAME_ERROR, "backup_snapshot_io_error", strlen("backup_snapshot_io_error"));
    if (stub_slow(b, "backup_cancelled", STUB_FRAME_ERROR) != 0) return 0;
    {
        static const char r[] = "{\"ok\":true,\"tag\":\"t\",\"timestamp\":1,\"files\":2,\"bytes\":3,\"err\":\"\"}";
        return stub_send_frame(177, r, sizeof r - 1);
    }
}

static int stub_op_restore(void *st, int fd, uint64_t t, const char *b) {
    (void)st; (void)fd; (void)t; (void)b;
    {
        static const char r[] = "{\"ok\":false,\"tag\":\"t\",\"restored\":0,\"err\":\"snapshot not found or restore failed\"}";
        return stub_send_frame(181, r, sizeof r - 1);
    }
}

static int stub_op_cleanup(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    {
        static const char r[] = "{\"ok\":true,\"removed_files\":4,\"removed_dirs\":1}";
        return stub_send_frame(33, r, sizeof r - 1);
    }
}

/* sdk.scan: a 100 KiB reply (a result the status poll must carry whole). */
static int stub_op_sdk_scan(void *st, int fd, uint64_t t) {
    size_t n = 100 * 1024;
    char *buf = malloc(n + 1);
    int rc;
    (void)st; (void)fd; (void)t;
    if (!buf) return -1;
    memset(buf, 'a', n);
    memcpy(buf, "{\"titles\":\"", 11);
    memcpy(buf + n - 2, "\"}", 2);
    buf[n] = '\0';
    rc = stub_send_frame(215, buf, n);
    free(buf);
    return rc;
}

static int stub_lo_fsck(void *st, int fd, uint64_t t, const char *b, uint64_t l) { return stub_op_fsck(st, fd, t, b, l); }
static int stub_lo_snapshot(void *st, int fd, uint64_t t, const char *b, uint64_t l) { (void)l; return stub_op_snapshot(st, fd, t, b); }
static int stub_lo_restore(void *st, int fd, uint64_t t, const char *b, uint64_t l) { (void)l; return stub_op_restore(st, fd, t, b); }
static int stub_lo_cleanup(void *st, int fd, uint64_t t, const char *b, uint64_t l) { return stub_op_cleanup(st, fd, t, b, l); }
static int stub_lo_sdk(void *st, int fd, uint64_t t, const char *b, uint64_t l) { (void)b; (void)l; return stub_op_sdk_scan(st, fd, t); }

static const mgmt_op_entry_t k_stub_ops[] = {
    {AVA1_JOB_OP_FSCK, 126, 127, 0, stub_lo_fsck},
    {AVA1_JOB_OP_BACKUP_SNAPSHOT, 176, 177, 0, stub_lo_snapshot},
    {AVA1_JOB_OP_BACKUP_RESTORE, 180, 181, 0, stub_lo_restore},
    {AVA1_JOB_OP_CLEANUP, 32, 33, 0, stub_lo_cleanup},
    {AVA1_JOB_OP_SDK_SCAN, 214, 215, 0, stub_lo_sdk},
};

/* The operations a data-layer test server serves: the real fs_jobs.c ones and the stubs. */
static void install_ops(void) {
    ava1_op_unregister_all();
    fsj_register_ops();
    (void)mgmt_rpc_install_ops(k_stub_ops, sizeof k_stub_ops / sizeof k_stub_ops[0]);
    __atomic_store_n(&fsj_test_file_delay_us, 0, __ATOMIC_SEQ_CST);
    fsj_test_cross_name = NULL;
}

/* Runs the reaper as if an hour had passed: finished jobs go, running ones stay. */
void ava1_test_reap_far(void) { ava1_job_reap(ava1_mono_ms() + 3600u * 1000u); }

static char g_cross_name[64];
/* A folder with this base name reports another device (a mount point); NULL or "" = off. */
void ava1_test_fsj_cross_name(const char *name) {
    if (!name || !name[0]) {
        fsj_test_cross_name = NULL;
        return;
    }
    snprintf(g_cross_name, sizeof g_cross_name, "%s", name);
    fsj_test_cross_name = g_cross_name;
}

static int g_cab_left;
static int cab_cancelled(void *arg) {
    (void)arg;
    return --g_cab_left < 0;
}

/* fsj_copy_atomic with a cancel after `blocks` blocks (negative: never). */
int ava1_test_copy_atomic(const char *src, const char *dst, int blocks) {
    fsj_hooks_t h = { cab_cancelled, NULL, NULL, NULL };
    g_cab_left = blocks < 0 ? 1 << 30 : blocks;
    return fsj_copy_atomic(src, dst, &h);
}

void ava1_test_fsj_delay_us(uint32_t us) { __atomic_store_n(&fsj_test_file_delay_us, us, __ATOMIC_SEQ_CST); }

/* ---- the data layer on the wire (Task 14) ----------------------------------------- */

/* The data server's hook: the data plane's own methods first (Task 19), then node.info. */
static int data_rpc(uint16_t method, const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap,
                    size_t *out_len) {
    int rc = ava1_data_rpc(method, body, body_len, out, cap, out_len);
    if (rc != -1) return rc;
    return rpc(method, body, body_len, out, cap, out_len);
}

/* The server with the real data hooks: uploads land under any absolute path. `port` 0 =
 * any (a restart passes the old one); `workers` nonzero fixes the apply pool's size. */
int ava1_test_server_start_data(const uint8_t secret[32], const char *peers_path, const char *jobs_dir,
                                uint32_t ping_ms, uint32_t dead_ms, uint32_t handshake_ms, uint32_t fsync_delay_us,
                                uint16_t port, uint8_t workers) {
    ava1_server_cfg_t cfg;
    ava1_data_cfg_t dc;
    int rc;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "%s", jobs_dir);
    dc.may_write = t_allow;
    dc.may_read = t_allow_read;
    dc.refuse_link = t_refuse_link;
    dc.same_device = t_same_device;
    dc.fsync_delay_us = fsync_delay_us;
    apply_opts(&dc);
    dc.workers_start = dc.workers_min = dc.workers_max = workers;
    ava1_test_set_same_device(1);
    __atomic_store_n(&ava1_send_test_fail_sends, 0, __ATOMIC_SEQ_CST); /* the download sender's knobs */
    __atomic_store_n(&ava1_send_test_fail_writer_starts, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&g_fault_id, UINT32_MAX - 1, __ATOMIC_SEQ_CST);
    __atomic_store_n(&ava1_copy_test_crash_before_delete, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&ava1_copy_test_delete_delay_ms, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&ava1_copy_test_delete_active, 0, __ATOMIC_SEQ_CST);
    if (ava1_data_start(&dc) != 0) return -100;
    install_ops();
    ava1_apply_fault = t_fault;
    memset(&cfg, 0, sizeof cfg);
    ava1_identity_from_secret(&cfg.identity, secret);
    strncpy(cfg.name, "C data server", sizeof cfg.name - 1);
    strncpy(cfg.peers_path, peers_path, sizeof cfg.peers_path - 1);
    cfg.port = port;
    cfg.bind_loopback = 1;
    cfg.ping_every_ms = ping_ms;
    cfg.dead_after_ms = dead_ms;
    cfg.handshake_ms = handshake_ms;
    cfg.rpc = data_rpc; /* the data plane's methods, then node.info */
    cfg.data = ava1_data_hooks();
    cfg.caps = AVA1_CAP_DATA_PLANE;
    rc = ava1_server_start(&cfg);
    if (rc != 0) {
        ava1_data_stop();
        return rc;
    }
    return (int)ava1_server_port();
}

void ava1_test_server_stop_data(void) {
    ava1_server_stop();
    ava1_data_stop();
}

#include "sony_api_lock.h"
int ava1_test_payload_stop(int conn_ms, int sony_ms) { return ava1_payload_stop(conn_ms, sony_ms); }
void ava1_test_sony_lock(void) { pthread_mutex_lock(&sony_api_lock); }
void ava1_test_sony_unlock(void) { pthread_mutex_unlock(&sony_api_lock); }

/* UINT32_MAX keeps a value. */
void ava1_test_data_delays(uint32_t open_ms, uint32_t map_ms) {
    if (open_ms != UINT32_MAX) ava1_data_test_open_delay_ms = open_ms;
    if (map_ms != UINT32_MAX) ava1_data_test_map_delay_ms = map_ms;
}

/* 1 attached, 0 parked, -1 not listed. */
int ava1_test_job_attached(const uint8_t id[16]) {
    ava1_job_t *j = ava1_job_find(id);
    int a;
    if (!j) return -1;
    pthread_mutex_lock(&j->cmu);
    a = j->attached;
    pthread_mutex_unlock(&j->cmu);
    ava1_job_put(j);
    return a;
}

static int listed(uint8_t tag) {
    uint8_t id[16];
    ava1_job_t *j;
    memset(id, tag, 16);
    j = ava1_job_find(id);
    if (j) ava1_job_put(j);
    return j != NULL;
}

static ava1_job_t *mk(uint8_t tag, const uint8_t *sid) {
    uint8_t id[16];
    memset(id, tag, 16);
    return ava1_job_create_attached(id, TEST_OWNER, sid);
}

/* Rulings 3 and 4. 0, or the step that failed. */
int ava1_test_reap_rules(const char *jobs_dir) {
    static const uint8_t SA[16] = { 0xa1 }, SB[16] = { 0xb1 }, SC[16] = { 0xc1 }, SD[16] = { 0xd1 };
    ava1_data_cfg_t dc;
    ava1_job_t *j;
    uint8_t id[16];
    int rc = 0;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "%s", jobs_dir);
    dc.park_ms = 300;
    if (ava1_data_start(&dc) != 0) return -100;
    /* Created detached: stamped parked at creation, so it ages out. */
    if (!(j = mk(0x31, NULL))) rc = -1;
    else ava1_job_put(j);
    /* Attached at creation (a driver, or on_job_open): never parked, survives the age. */
    if (!rc && !(j = mk(0x32, SA))) rc = -2;
    else if (!rc) ava1_job_put(j);
    /* Attached, then its session ended: parked, reaped. */
    if (!rc && !(j = mk(0x33, SB))) rc = -3;
    else if (!rc) {
        ava1_job_put(j);
        ava1_job_park_session(SB);
    }
    if (!rc) {
        ava1_platform_sleep_ms(2300); /* > park age + two housekeeping ticks */
        if (listed(0x31)) rc = -4;
        else if (!listed(0x32)) rc = -5;
        else if (listed(0x33)) rc = -6;
    }
    /* find_attach under the table lock: a parked job taken over is never collected. */
    if (!rc && !(j = mk(0x34, SC))) rc = -7;
    else if (!rc) {
        ava1_job_put(j);
        ava1_job_park_session(SC);
        memset(id, 0x34, 16);
        j = ava1_job_find_attach(id, SD);
        ava1_job_reap(ava1_mono_ms() + 3600u * 1000u);
        if (!j) rc = -8;
        else if (!listed(0x34)) rc = -9;
        if (j) ava1_job_put(j);
        ava1_job_park_session(SD);
        ava1_job_reap(ava1_mono_ms() + 3600u * 1000u);
        if (!rc && listed(0x34)) rc = -10;
    }
    ava1_data_stop();
    return rc;
}

/* N3: a job a cancel unlisted, still referenced by the caller and one other holder. */
int ava1_test_retire_unlisted(void) {
    ava1_data_cfg_t dc;
    ava1_job_t *j, *other;
    uint8_t id[16];
    int rc = 0;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "/tmp/ava1-retire-unused");
    if (ava1_data_start(&dc) != 0) return -100;
    memset(id, 0x41, 16);
    j = ava1_job_create_attached(id, TEST_OWNER, TEST_OWNER);
    other = j ? ava1_job_find(id) : NULL;
    if (!j || !other) rc = -1;
    if (!rc) {
        ava1_job_free_one(id); /* unlisted; refs: ours and `other` */
        if (ava1_job_retire(j) == 0) {
            rc = -2; /* it freed the job under `other` (which must not be touched now) */
        } else {
            if (memcmp(other->id, id, 16) != 0) rc = -3;
            ava1_job_put(other);
        }
    }
    ava1_data_stop();
    return rc;
}

/* out[0] = bytes the apply engine received, out[1] = lane frames held. -1: not listed. */
int ava1_test_job_counts(const uint8_t id[16], uint64_t out[2]) {
    ava1_job_t *j = ava1_job_find(id);
    ava1_inframe_t *f;
    if (!j) return -1;
    pthread_mutex_lock(&j->mu);
    out[0] = j->bytes_received;
    pthread_mutex_unlock(&j->mu);
    out[1] = 0;
    pthread_mutex_lock(&j->cmu);
    for (f = j->held_head; f; f = f->next) out[1]++;
    pthread_mutex_unlock(&j->cmu);
    ava1_job_put(j);
    return 0;
}

/* Fix round 1 knobs, by name. 0 = done, -1 = unknown name. */
int ava1_test_data_knob(const char *name, uint32_t v) {
    ava1_data_cfg_t *c = (ava1_data_cfg_t *)ava1_data_cfg(); /* a static, not const: tests only */
    if (!strcmp(name, "ack_fail")) ava1_data_test_ack_fail = (int)v ? -(int)v : 0;
    else if (!strcmp(name, "feeder_fail")) ava1_data_test_feeder_fail = (int)v;
    else if (!strcmp(name, "feed_delay_ms")) ava1_data_test_feed_delay_ms = v;
    else if (!strcmp(name, "reserve_fail")) ava1_data_test_reserve_fail = (int)v;
    else if (!strcmp(name, "lane_alloc_fail")) ava1_data_test_lane_alloc_fail = (int)v;
    else if (!strcmp(name, "fb_force")) ava1_data_test_fb_force = (int)v;
    else if (!strcmp(name, "park_ms")) __atomic_store_n(&c->park_ms, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "deny_write")) __atomic_store_n(&g_deny_write, (int)v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "budget_free")) {
        (void)ava1_budget_take(UINT64_MAX, 0);
        ava1_budget_give(v);
    }
    else if (!strcmp(name, "copy_walk_delay_ms")) __atomic_store_n(&ava1_copy_test_walk_delay_ms, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "copy_delete_delay_ms")) __atomic_store_n(&ava1_copy_test_delete_delay_ms, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "copy_crash_before_delete"))
        __atomic_store_n(&ava1_copy_test_crash_before_delete, (int)v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "progress_ms")) __atomic_store_n(&c->progress_ms, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "resume_progress_ms")) __atomic_store_n(&c->resume_progress_ms, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "ctl_cap")) __atomic_store_n(&c->ctl_cap, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "send_fail")) __atomic_store_n(&ava1_send_test_fail_sends, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "writer_start_fail"))
        __atomic_store_n(&ava1_send_test_fail_writer_starts, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "fd_budget")) __atomic_store_n(&ava1_data_test_fd_budget, v, __ATOMIC_SEQ_CST);
    else if (!strcmp(name, "fd_peak_reset")) {
        ava1_pend_peak_reset();
        __atomic_store_n(&ava1_data_test_cal_peak, 0, __ATOMIC_SEQ_CST);
    }
    else if (!strcmp(name, "chunk_bytes")) __atomic_store_n(&ava1_send_test_chunk_bytes, v, __ATOMIC_SEQ_CST);
    else return -1;
    return 0;
}

int ava1_test_copy_walk_active(void) { return __atomic_load_n(&ava1_copy_test_walk_active, __ATOMIC_ACQUIRE); }
int ava1_test_copy_delete_active(void) { return __atomic_load_n(&ava1_copy_test_delete_active, __ATOMIC_ACQUIRE); }

/* Fix round 1, minor 5: a job unlisted (reaped) while someone still holds it keeps its id
 * until it is destroyed: a create of the same id fails until then. 0 or the failed step. */
int ava1_test_retiring_blocks_reopen(void) {
    ava1_data_cfg_t dc;
    ava1_job_t *j, *held, *again;
    uint8_t id[16];
    int rc = 0;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "/tmp/ava1-retiring-unused");
    if (ava1_data_start(&dc) != 0) return -100;
    memset(id, 0x42, 16);
    j = ava1_job_create(id, TEST_OWNER); /* detached: parked from creation */
    held = j ? ava1_job_find(id) : NULL; /* someone still running it (a hook, a destroy) */
    if (!j || !held) rc = -1;
    if (!rc) {
        ava1_job_put(j);
        ava1_job_reap(ava1_mono_ms() + 3600u * 1000u); /* unlisted; `held` keeps it alive */
        again = ava1_job_create(id, TEST_OWNER);
        if (again) {
            rc = -2; /* a second job over the same directory while the first still runs */
            ava1_job_put(again);
        }
        ava1_job_put(held); /* destroyed now: the id is free */
        if (!rc && !(again = ava1_job_create(id, TEST_OWNER))) rc = -3;
        else if (!rc) ava1_job_put(again);
    }
    ava1_data_stop();
    return rc;
}

/* Tests: high-water marks of open pending fds (apply) and calibrate-held fds. */
uint32_t ava1_test_fd_peak(int which) { return which ? __atomic_load_n(&ava1_data_test_cal_peak, __ATOMIC_SEQ_CST) : ava1_pend_peak(); }

/* Chunk bytes the download sender has queued since the counter was last set (knob
 * "chunk_bytes"). */
uint64_t ava1_test_send_chunk_bytes(void) { return __atomic_load_n(&ava1_send_test_chunk_bytes, __ATOMIC_SEQ_CST); }

/* A copy's in-process put: a Chunk/Bundle that does not decode is freed and its reserved
 * bytes go back (0 = both hold, else the failing step). */
int ava1_test_copy_put_decode_failure(void) {
    ava1_data_cfg_t dc;
    ava1_job_t *j;
    uint8_t id[16];
    uint8_t type[2] = { AVA1_TYPE_CHUNK, AVA1_TYPE_BUNDLE };
    int rc = 0, i;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "/tmp/ava1-copyput-unused");
    if (ava1_data_start(&dc) != 0) return -100;
    memset(id, 0x43, 16);
    j = ava1_job_create(id, TEST_OWNER);
    if (!j) rc = -1;
    else {
        pthread_mutex_lock(&j->mu);
        j->credit = 1u << 20;
        j->outstanding = 0;
        pthread_mutex_unlock(&j->mu);
        for (i = 0; i < 2 && !rc; i++) {
            uint8_t *msg = calloc(1, 8); /* too short to be either message */
            size_t before;
            if (!msg) {
                rc = -2;
                break;
            }
            pthread_mutex_lock(&j->mu);
            before = j->outstanding;
            pthread_mutex_unlock(&j->mu);
            if (ava1_copy_put(j, type[i], msg, 8) != -EIO) rc = -3 - i * 10;
            pthread_mutex_lock(&j->mu);
            if (j->outstanding != before) rc = -4 - i * 10;
            pthread_mutex_unlock(&j->mu);
        }
        pthread_mutex_lock(&j->mu);
        j->credit = 0; /* nothing was granted from the budget */
        pthread_mutex_unlock(&j->mu);
        ava1_job_put(j);
    }
    ava1_data_stop();
    return rc;
}

/* A FILE_RETRY(changed) reaching a copy's emit hook records the failure for the job
 * thread, with the user-facing message (0 = it does, else the failing step). */
int ava1_test_copy_retry_changed_message(void) {
    ava1_data_cfg_t dc;
    ava1_job_t *j;
    ava1_file_retry_t r;
    uint8_t id[16], b[64];
    ava1_w_t w;
    int rc = 0;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "/tmp/ava1-copyretry-unused");
    if (ava1_data_start(&dc) != 0) return -100;
    memset(id, 0x44, 16);
    j = ava1_job_create(id, TEST_OWNER);
    if (!j) rc = -1;
    else {
        uint16_t reasons[2] = { AVA1_RETRY_CHANGED, AVA1_RETRY_VERIFY };
        const char *want[2] = { "source changed while copying", "a copied file did not verify" };
        int i;
        for (i = 0; i < 2 && !rc; i++) {
            memset(&r, 0, sizeof r);
            memcpy(r.job_id, id, 16);
            r.reason = reasons[i];
            ava1_w_init(&w, b, sizeof b);
            if (ava1_file_retry_encode(&r, &w) != 0) rc = -2;
            else {
                pthread_mutex_lock(&j->mu);
                j->final_status = 0;
                j->message[0] = 0;
                pthread_mutex_unlock(&j->mu);
                ava1_copy_emit(j, AVA1_TYPE_FILE_RETRY, 0, b, w.len);
                pthread_mutex_lock(&j->mu);
                if (j->final_status != AVA1_ERR_VERIFY) rc = -3 - i * 10;
                else if (strcmp(j->message, want[i]) != 0) rc = -4 - i * 10;
                pthread_mutex_unlock(&j->mu);
            }
        }
        ava1_job_put(j);
    }
    ava1_data_stop();
    return rc;
}
