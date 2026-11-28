#ifndef _GNU_SOURCE
#define _GNU_SOURCE /* pthread_getattr_np on glibc */
#endif
/* ps5upload's half of the test shim: the management dispatcher (mgmt_rpc.c) over stub handlers, the
 * job.run operations (fs_jobs.c and stubs), the shared path policy, the payload stop sequence and the
 * Sony API lock. It plugs into the core shim (test_shim.c, a copy of the AVA1 library's
 * third_party/ava1/crates/ava1-ctest/csrc/test_shim.c) through test_shim.h's hooks, which
 * ava1_test_app_install fills in. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "ava1_stop.h"
#include "fs_jobs.h"
#include "mgmt_rpc.h"
#include "net_probe.h"
#include "path_policy.h"
#include "test_shim.h"

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

/* A legacy handler that answers an empty ack or an ERROR frame, by the {"path":...} it gets. */
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
        "\"instance_id\":18446744073709551000,\"runtime_port\":9120,\"shutdown\":0,\"startup_reason\":2,"
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

#define STUB_RUN(name, helper) \
    static int run_##name(const uint8_t *q, uint32_t n, mgmt_ctx_t *cx) { return helper(q, n, cx, name); }
STUB_RUN(stub_volumes, mgmt_call_text)
STUB_RUN(stub_mkdir, mgmt_call_text)
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

static void fire_payload_stop(void *arg) {
    (void)arg;
    (void)ava1_payload_stop(2000, 2000);
}

/* Off by default: only the Task 8 shutdown tests answer node.shutdown here (the shape of runtime.c's
 * handle_node_shutdown); every other test reaches its installed management table. */
static int g_intercept_shutdown;
void ava1_test_intercept_shutdown(int on) { __atomic_store_n(&g_intercept_shutdown, on, __ATOMIC_SEQ_CST); }

int ava1_test_exit_flush_create_fails(int fail) {
    int rc;
    ava1_exit_test_fail_create = fail;
    rc = ava1_exit_flush(500);
    ava1_exit_test_fail_create = 0;
    return rc;
}

/* ---- P3 Task 5: job.run operations wrapped around stub legacy handlers -------------------- */

/* Hold operations until their initial running reply has been read by a test. */
static int g_stub_op_hold;

/* A slow stub: `loops` rounds of 20 ms, each reporting one unit of progress and honouring
 * job.cancel the way backup.c does (mgmt_op_cancelled / mgmt_op_progress). */
static int stub_slow(const char *b, const char *cancel_token, int cancel_frame_type) {
    uint32_t i, loops = 0;
    const char *p = strstr(b, "\"loops\":");
    if (p) loops = (uint32_t)strtoul(p + 8, NULL, 10);
    while (__atomic_load_n(&g_stub_op_hold, __ATOMIC_SEQ_CST)) {
        if (mgmt_op_cancelled()) return stub_send_frame((uint16_t)cancel_frame_type, cancel_token, strlen(cancel_token));
        usleep(1000);
    }
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

#include "sony_api_lock.h"
int ava1_test_payload_stop(int conn_ms, int sony_ms) { return ava1_payload_stop(conn_ms, sony_ms); }
void ava1_test_sony_lock(void) { pthread_mutex_lock(&sony_api_lock); }
void ava1_test_sony_unlock(void) { pthread_mutex_unlock(&sony_api_lock); }

/* Review S2: the data layer's refuse_link hook, wired to the real shared policy when a test names a protected
 * directory (NULL turns it off). */
static int g_protect_links;
void ava1_test_set_protected(const char *dir) {
    path_policy_set_protected(dir);
    __atomic_store_n(&g_protect_links, dir != NULL, __ATOMIC_SEQ_CST);
}
static int t_refuse_link(const char *p) { return __atomic_load_n(&g_protect_links, __ATOMIC_SEQ_CST) && path_tree_op_refused(p); }

/* Every method past node.info: the Task 8 shutdown intercept, then the installed management table. */
static int app_rpc(uint16_t method, const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap,
                   size_t *out_len) {
    if (method == AVA1_METHOD_NODE_SHUTDOWN && __atomic_load_n(&g_intercept_shutdown, __ATOMIC_SEQ_CST)) {
        if (ava1_shutdown_defer(300, fire_payload_stop, NULL) != 0) return AVA1_ERR_INTERNAL;
        *out_len = 0; /* SPEC: node.shutdown answers an empty body (mgmt_call_empty) */
        return AVA1_STATUS_OK;
    }
    return mgmt_rpc_dispatch(method, body, body_len, out, cap, out_len);
}

/* A folder size's read rule, as the helper sets it (ava1_glue.c may_size: fs.read's rule): the
 * trust store and what is in it refused, a folder above it allowed whatever the data plane's
 * may_read says (on the console that one refuses every folder above the store). */
static int t_size_may_read(const char *p) { return !path_in_protected(p) && !strstr(p, "/ps5-denied/"); }

/* The operations a data-layer test server serves: the real fs_jobs.c ones and the stubs. */
static void app_data_started(void) {
    __atomic_store_n(&g_stub_op_hold, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&fsj_size_may_read, t_size_may_read, __ATOMIC_RELEASE);
    fsj_register_ops();
    (void)mgmt_rpc_install_ops(k_stub_ops, sizeof k_stub_ops / sizeof k_stub_ops[0]);
    __atomic_store_n(&fsj_test_file_delay_us, 0, __ATOMIC_SEQ_CST);
    fsj_test_cross_name = NULL;
}

static int app_knob(const char *name, uint32_t v) {
    if (!strcmp(name, "op_hold")) __atomic_store_n(&g_stub_op_hold, (int)v, __ATOMIC_SEQ_CST);
    else return -1;
    return 0;
}

/* Called by the crate before every server start (src/lib.rs); idempotent. */
void ava1_test_app_install(void) {
    ava1_test_hooks.rpc = app_rpc;
    ava1_test_hooks.mgmt_installed = mgmt_rpc_installed;
    __atomic_store_n(&ava1_test_hooks.refuse_link, t_refuse_link, __ATOMIC_SEQ_CST);
    ava1_test_hooks.data_started = app_data_started;
    ava1_test_hooks.knob = app_knob;
}
