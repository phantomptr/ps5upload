/* P3 Task 4 host harness: the REAL native filesystem runners (payload/src/mgmt_fs.c) behind a
 * temp-directory policy, plus stub legacy handlers for the node/log/net methods whose runners
 * (mgmt_call_klog, mgmt_call_syslog, mgmt_call_text_keep, mgmt_call_empty) are the code under test.
 * The real table (payload/src/mgmt_table.def) is built into runtime.c and is not compiled here. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_gen.h"
#include "cross_device.h"
#include "mgmt_fs.h"
#include "mgmt_rpc.h"
#include "path_policy.h"

#define FRAME_ERROR 3u

static char g_root[512];
static uint32_t g_counted, g_shutdowns, g_unsafe_seen;
static int g_fake_dev; /* 1: a path containing "/mnt2/" is on another device */
static uint32_t g_klog_avail, g_syslog_len;

static int g_allow_dev; /* the xdev symlink test renames into /dev: the guard must refuse before any rename */

/* The pure string rule of this test's "allowed roots": the temp root (and /dev when asked). */
static int lexical_mine(const char *path) {
    size_t n = strlen(g_root);
    if (path[0] != '/' || strstr(path, "/../") || strstr(path, "/./")) return 0;
    if (g_allow_dev && strncmp(path, "/dev/", 5) == 0) return 1;
    return g_root[0] && strncmp(path, g_root, n) == 0 && (path[n] == '/' || path[n] == '\0');
}

/* The REAL symlink-safe resolution (payload/src/path_policy.c, what runtime.c's is_path_allowed runs). */
static int mine(const char *path) { return path_resolve_allowed(path, lexical_mine); }

static int t_write_allowed(const char *path) { return mine(path); }

static int t_read_allowed(const char *path, int unsafe_read) {
    char sys[600];
    snprintf(sys, sizeof sys, "%s/sys/", g_root);
    if (unsafe_read) __atomic_add_fetch(&g_unsafe_seen, 1, __ATOMIC_SEQ_CST);
    /* The system tree under the root is readable only with the unsafe flag. */
    if (strncmp(path, sys, strlen(sys)) == 0) return unsafe_read && mine(path);
    return mine(path);
}

static void t_count(void) { __atomic_add_fetch(&g_counted, 1, __ATOMIC_SEQ_CST); }

/* With fake_dev set both lookups are injected; otherwise p.dev_of/src_dev_of are NULL (the real stat/lstat). */
static int t_dev_of(const char *path, unsigned long long *out) {
    if (g_fake_dev == 3) return -1; /* every device lookup fails (the guard's unknown case) */
    if (g_fake_dev == 2) { /* a link named "lnk" points INTO the other device: stat() (follows it) says 2 */
        *out = (strstr(path, "/mnt2") || strstr(path, "/lnk")) ? 2u : 1u;
        return 0;
    }
    if (g_fake_dev) {
        *out = strstr(path, "/mnt2") ? 2u : 1u;
        return 0;
    }
    return xdev_stat_dev(path, out);
}
static int t_src_dev_of(const char *path, unsigned long long *out) {
    if (g_fake_dev == 2) { /* lstat(): the link itself lives on device 1, whatever it points at */
        *out = strstr(path, "/mnt2") ? 2u : 1u;
        return 0;
    }
    if (g_fake_dev) return t_dev_of(path, out);
    return xdev_lstat_dev(path, out);
}

static int send_frame(uint16_t type, const void *body, uint64_t len) {
    return mgmt_capture_active() ? mgmt_capture_frame(type, body, len) : -1;
}

/* node.shutdown */
static int h_shutdown(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    __atomic_add_fetch(&g_shutdowns, 1, __ATOMIC_SEQ_CST);
    return send_frame(23, "{}", 2);
}

/* node.cleanup */
static int h_cleanup(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)l;
    if (strstr(b, "denied")) return send_frame(FRAME_ERROR, "cleanup_path_denied", 19);
    if (!strstr(b, "path")) return send_frame(FRAME_ERROR, "cleanup_missing_path", 20);
    {
        static const char ok[] = "{\"ok\":true,\"path\":\"/data/x\",\"removed_files\":2,\"removed_dirs\":1}";
        return send_frame(7, ok, sizeof ok - 1);
    }
}

/* log.klog: the real handler's clamping (16 KiB default, 64 KiB cap, a request only when < 256 bytes). */
static int h_klog(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    size_t max = 16 * 1024, n;
    char *buf;
    int rc;
    (void)st; (void)fd; (void)t;
    if (b && l > 0 && l < 256) {
        const char *p = strstr(b, "\"max_bytes\"");
        if (p && (p = strchr(p, ':')) != NULL) {
            long long v = atoll(p + 1);
            if (v > 0) {
                max = (size_t)v;
                if (max > 64 * 1024) max = 64 * 1024;
            }
        }
    }
    if (g_klog_avail == 0xffffffffu) return send_frame(FRAME_ERROR, "open_klog_failed", 16);
    n = g_klog_avail < max ? g_klog_avail : max;
    buf = malloc(n + 1);
    if (!buf) return -1;
    memset(buf, 'k', n);
    rc = send_frame(109, buf, n);
    free(buf);
    return rc;
}

/* log.syslog: g_syslog_len bytes, byte i = 'a' + i % 26. */
static int h_syslog(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    size_t i, n = g_syslog_len;
    char *buf;
    int rc;
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    if (g_syslog_len == 0xffffffffu) return send_frame(FRAME_ERROR, "syslog_tail_sysctl_errno_5", 26);
    buf = malloc(n + 1);
    if (!buf) return -1;
    for (i = 0; i < n; i++) buf[i] = (char)('a' + i % 26);
    rc = send_frame(145, buf, n);
    free(buf);
    return rc;
}

static int h_ifaces(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char body[] = "{\"interfaces\":[{\"name\":\"em0\",\"mac\":\"aa:bb\",\"ipv4\":\"10.0.0.2\",\"mtu\":1500,\"flags\":3}],\"source\":\"getifaddrs\"}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return send_frame(111, body, sizeof body - 1);
}

/* net.reach: a failure that carries data the caller needs. */
static int h_reach(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char bad[] = "{\"ok\":false,\"timed_out\":true,\"errno\":0,\"err\":\"timed out\",\"ms\":3000}";
    static const char good[] = "{\"ok\":true,\"ms\":4}";
    (void)st; (void)fd; (void)t; (void)l;
    if (strstr(b, "unreachable")) return send_frame(149, bad, sizeof bad - 1);
    if (!strstr(b, "host")) return send_frame(149, "{\"ok\":false,\"err\":\"bad_request\"}", 32);
    return send_frame(149, good, sizeof good - 1);
}

static int h_speed(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return send_frame(123, "{\"ok\":true}", 11);
}

/* fs.mount_pkg: the mount failed; the body carries the code and the mount point. */
static int h_pkg(void *st, int fd, uint64_t t, const char *b, uint64_t l) {
    static const char bad[] = "{\"ok\":false,\"code\":-2147352567,\"mount_point\":\"/mnt/ps5upload/x.pkg.mount\"}";
    (void)st; (void)fd; (void)t; (void)b; (void)l;
    return send_frame(125, bad, sizeof bad - 1);
}

#define RUN(name, helper) \
    static int run_##name(const uint8_t *q, uint32_t n, mgmt_ctx_t *cx) { return helper(q, n, cx, name); }
RUN(h_shutdown, mgmt_call_empty)
RUN(h_cleanup, mgmt_call_text)
RUN(h_klog, mgmt_call_tail)
RUN(h_syslog, mgmt_call_tail)
RUN(h_ifaces, mgmt_call_text)
RUN(h_reach, mgmt_call_probe)
RUN(h_speed, mgmt_call_text)
RUN(h_pkg, mgmt_call_text_keep)

static const mgmt_entry_t k_table[] = {
    {AVA1_METHOD_NODE_SHUTDOWN, 22, 23, 0, run_h_shutdown},
    {AVA1_METHOD_NODE_CLEANUP, 32, 33, 0, run_h_cleanup},
    {AVA1_METHOD_LOG_KLOG, 108, 109, 0, run_h_klog},
    {AVA1_METHOD_LOG_SYSLOG, 144, 145, 0, run_h_syslog},
    {AVA1_METHOD_NET_INTERFACES, 110, 111, 0, run_h_ifaces},
    {AVA1_METHOD_NET_REACH, 148, 149, 0, run_h_reach},
    {AVA1_METHOD_NET_SPEEDTEST, 122, 123, 0, run_h_speed},
    {AVA1_METHOD_FS_MOUNT_PKG, 124, 125, 0, run_h_pkg},
    {AVA1_METHOD_FS_LIST, 36, 37, 0, mgmt_run_fs_list},
    {AVA1_METHOD_FS_STAT, 36, 37, 0, mgmt_run_fs_stat},
    {AVA1_METHOD_FS_MKDIR, 46, 47, 0, mgmt_run_fs_mkdir},
    {AVA1_METHOD_FS_RENAME, 42, 43, 0, mgmt_run_fs_rename},
    {AVA1_METHOD_FS_CHMOD, 44, 45, 0, mgmt_run_fs_chmod},
    {AVA1_METHOD_FS_READ, 48, 49, 0, mgmt_run_fs_read},
    {AVA1_METHOD_FS_WRITE, 130, 131, 0, mgmt_run_fs_write},
};

int ava1_test_mgmtfs_install(const char *root) {
    mgmt_fs_policy_t p;
    memset(&p, 0, sizeof p);
    snprintf(g_root, sizeof g_root, "%s", root);
    {
        char prot[700];
        /* the trust store of this test console: <root>/d/ava (an ancestor, <root>/d, exists to be refused too) */
        snprintf(prot, sizeof prot, "%s/d/ava", root);
        path_policy_set_protected(prot);
    }
    p.write_allowed = t_write_allowed;
    p.read_allowed = t_read_allowed;
    p.count = t_count;
    p.dev_of = t_dev_of;
    p.src_dev_of = t_src_dev_of;
    mgmt_fs_set_policy(&p);
    g_counted = g_shutdowns = g_unsafe_seen = 0;
    g_fake_dev = 0;
    g_allow_dev = 0;
    g_klog_avail = g_syslog_len = 0;
    return mgmt_rpc_install(k_table, sizeof k_table / sizeof k_table[0], NULL, NULL, NULL);
}

void ava1_test_mgmtfs_uninstall(void) {
    path_policy_set_protected(NULL);
    g_counted = g_shutdowns = g_unsafe_seen = 0;
    mgmt_fs_set_policy(NULL);
    (void)mgmt_rpc_install(NULL, 0, NULL, NULL, NULL);
}

void ava1_test_mgmtfs_allow_dev(int on) { g_allow_dev = on; }

void ava1_test_mgmtfs_set(int fake_dev, uint32_t klog_avail, uint32_t syslog_len) {
    g_fake_dev = fake_dev;
    g_klog_avail = klog_avail;
    g_syslog_len = syslog_len;
}

void ava1_test_mgmtfs_stats(uint32_t *counted, uint32_t *shutdowns, uint32_t *unsafe_seen) {
    *counted = __atomic_load_n(&g_counted, __ATOMIC_SEQ_CST);
    *shutdowns = __atomic_load_n(&g_shutdowns, __ATOMIC_SEQ_CST);
    *unsafe_seen = __atomic_load_n(&g_unsafe_seen, __ATOMIC_SEQ_CST);
}

int ava1_test_path_in_protected(const char *p) { return path_in_protected(p); }
int ava1_test_path_contains_protected(const char *p) { return path_contains_protected(p); }
int ava1_test_path_tree_op_refused(const char *p) { return path_tree_op_refused(p); }
