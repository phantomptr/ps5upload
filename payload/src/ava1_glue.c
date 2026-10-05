/* AVA1 on the console: identity and peers under /data/ps5upload/ava, the launching
 * engine trusted through the ELF's trust slot. Pairing opens by itself for 5 minutes
 * only while nothing is paired; otherwise a paired device opens it (pairing.open). */
#include "ava1_glue.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <time.h>

#include "ava1_aead.h"
#include "ava1_data.h"
#include "ava1_events.h"
#include "ava1_gen.h"
#include "ava1_journal.h"
#include "ava1_noise.h"
#include "ava1_server.h"
#include "ava1_store.h"
#include "ava1_trust.h"
#include "config.h"
#include "fs_jobs.h"
#include "mgmt_rpc.h"
#include "cross_device.h"
#include "path_policy.h" /* payload/include, not next to the AVA1 sources */
#include "monocypher.h"
#include "ps5_firmware.h"
#include "runtime.h"

#define AVA1_DIR "/data/ps5upload/ava"
#define AVA1_JOBS AVA1_DIR "/jobs"

/* Set once, before the server starts (so before any rpc call): the data layer is running.
 * Without it the data plane's methods are unknown here, matching the missing CAP. */
static int g_data_on;

/* Data-plane jobs (upload root, download root, job.copy source and destination) walk trees, so they refuse the
 * trust store AND its ancestors (path_policy.h: path_tree_op_refused). */
static int may_write(const char *p) { return is_path_allowed(p) && !path_tree_op_refused(p); }

/* The same rule the management read handlers use (runtime.c): the writable allowlist, or a system
 * partition read when the peer asked for an unsafe read. */
static int may_read(const char *p, int unsafe_read) {
    if (path_tree_op_refused(p)) return 0;
    return is_path_allowed(p) || (unsafe_read && is_safe_unsafe_read_path(p));
}

/* 1 same device, 0 crosses (refuse: a cross-device rename panics this kernel), -1 unknown.
 * Callers treat anything but 1 as a refusal: unknown is never "same" (review 007 #4). */
static int same_device(const char *from, const char *to_dir) {
    unsigned long long a, b;
    if (xdev_lstat_dev(from, &a) != 0 || xdev_stat_dev(to_dir, &b) != 0) return -1;
    return a == b ? 1 : 0;
}

static void on_pair_request(const char *peer_name, uint32_t code) {
    char msg[160];
    /* The code exists only on this screen (SPEC.md 5.5): the user types it into the app. */
    snprintf(msg, sizeof msg, "PS5Upload pairing: enter %06u in the app (%s)", (unsigned)code, peer_name);
    pop_notification(msg);
}

static void on_log(const char *msg) { fprintf(stderr, "%s\n", msg); }

static uint64_t mono_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000u + (uint64_t)ts.tv_nsec / 1000u;
}

/* crypto.bench: the frame AEAD in use (ava1_seal / ava1_open) on `mib` 1 MiB frames in
 * memory, timed on this console (spec risk table), and which ChaCha20 path it chose. */
static int crypto_bench(const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap, size_t *out_len) {
    const size_t MIB = 1u << 20;
    ava1_crypto_bench_t q;
    ava1_crypto_bench_result_t r;
    ava1_w_t w;
    uint8_t key[32], mac[16], *buf, *ct;
    const char *backend = ava1_aead_backend();
    uint64_t t0, copy;
    unsigned i, mib;
    int bad = 0;
    if (ava1_crypto_bench_decode(body, body_len, &q) != 0) return AVA1_ERR_PROTOCOL;
    mib = q.mib == 0 ? 1u : (q.mib > 256 ? 256u : q.mib);
    buf = malloc(MIB);
    ct = malloc(MIB);
    if (!buf || !ct) {
        free(buf);
        free(ct);
        return AVA1_ERR_INTERNAL;
    }
    memset(buf, 0x5a, MIB);
    memset(key, 0x11, sizeof key);
    memset(&r, 0, sizeof r);
    t0 = mono_us();
    for (i = 0; i < mib; i++) ava1_seal(key, i, NULL, 0, buf, MIB, mac);
    r.micros = mono_us() - t0;
    /* Opening needs the same frame every round: copy it in, and take the copies' time out. */
    memcpy(ct, buf, MIB);
    t0 = mono_us();
    for (i = 0; i < mib; i++) {
        memcpy(buf, ct, MIB);
        __asm__ __volatile__("" : : "r"(buf) : "memory"); /* keep every copy */
    }
    copy = mono_us() - t0;
    t0 = mono_us();
    for (i = 0; i < mib; i++) {
        memcpy(buf, ct, MIB);
        bad |= ava1_open(key, mib - 1, NULL, 0, buf, MIB, mac);
    }
    r.open_micros = mono_us() - t0;
    r.open_micros = r.open_micros > copy ? r.open_micros - copy : 1;
    free(buf);
    free(ct);
    if (bad) return AVA1_ERR_INTERNAL;
    r.bytes = (uint64_t)mib << 20;
    r.has_open_micros = 1;
    r.has_backend = 1;
    r.backend = (const uint8_t *)backend;
    r.backend_len = (uint16_t)strlen(backend);
    ava1_w_init(&w, out, cap);
    if (ava1_crypto_bench_result_encode(&r, &w) != 0) return AVA1_ERR_INTERNAL;
    *out_len = w.len;
    return AVA1_STATUS_OK;
}

/* The firmware version, from kern.version: the same source as the STATUS frame's
 * ps5_kernel, which the app parses the same way. */
static void read_firmware(char *out, size_t cap) {
    char kv[256];
    size_t len = sizeof kv - 1;
    memset(kv, 0, sizeof kv);
    if (sysctlbyname("kern.version", kv, &len, NULL, 0) != 0) kv[0] = '\0';
    kv[sizeof kv - 1] = '\0';
    ps5_firmware_from_kernel(kv, out, cap);
}

static int rpc(uint16_t method, const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap,
               size_t *out_len) {
    static const char version[] = PS5UPLOAD2_VERSION;
    ava1_node_info_t ni;
    ava1_w_t w;
    char firmware[64];
    /* First, so the data plane's methods win over the fallbacks below. */
    if (g_data_on) {
        int rc = ava1_data_rpc(method, body, body_len, out, cap, out_len);
        if (rc != -1) return rc;
    }
    if (method == AVA1_METHOD_CRYPTO_BENCH) return crypto_bench(body, body_len, out, cap, out_len);
    /* The management methods (4 and up): the management handlers behind the capture sink. An
     * unknown method answers ERR_UNKNOWN_METHOD from there. */
    if (method != AVA1_METHOD_NODE_INFO) return mgmt_rpc_dispatch(method, body, body_len, out, cap, out_len);
    read_firmware(firmware, sizeof firmware);
    memset(&ni, 0, sizeof ni);
    ni.version = (const uint8_t *)version;
    ni.version_len = (uint16_t)(sizeof version - 1);
    ni.platform = (const uint8_t *)"ps5";
    ni.platform_len = 3;
    ni.name = (const uint8_t *)"PS5";
    ni.name_len = 3;
    ni.has_firmware = 1;
    ni.firmware = (const uint8_t *)firmware;
    ni.firmware_len = (uint16_t)strlen(firmware);
    ava1_w_init(&w, out, cap);
    if (ava1_node_info_encode(&ni, &w) != 0) return AVA1_ERR_INTERNAL;
    *out_len = w.len;
    return AVA1_STATUS_OK;
}

int ava1_payload_start(void) {
    ava1_server_cfg_t cfg;
    uint8_t launcher[32], token[16];
    memset(&cfg, 0, sizeof cfg);
    if (mkdir("/data/ps5upload", 0755) != 0 && errno != EEXIST) return -errno;
    if (mkdir(AVA1_DIR, 0755) != 0 && errno != EEXIST) return -errno;
    if (ava1_identity_load_or_create(AVA1_DIR "/identity", &cfg.identity) != 0) return -EIO;
    snprintf(cfg.peers_path, sizeof cfg.peers_path, "%s", AVA1_DIR "/peers");
    snprintf(cfg.name, sizeof cfg.name, "%s", "PS5");
    cfg.port = AVA1_DEFAULT_PORT;
    cfg.ping_every_ms = 2000;
    cfg.dead_after_ms = 12000; /* SPEC.md section 6: the ping stays at 2 s */
    cfg.handshake_ms = 10000;
    cfg.pairing_window_s = 300;
    cfg.on_pair_request = on_pair_request;
    cfg.rpc = rpc;
    cfg.log = on_log;
    {
        ava1_data_cfg_t dc;
        int rc;
        memset(&dc, 0, sizeof dc);
        snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "%s", AVA1_JOBS);
        dc.may_write = may_write;
        dc.may_read = may_read;
        dc.refuse_link = path_tree_op_refused;
        dc.same_device = same_device;
        if (mkdir(AVA1_JOBS, 0755) != 0 && errno != EEXIST) {
            /* Not fatal: the server keeps working without the data plane. */
            on_log("ava1: cannot create the jobs folder; transfers are unavailable");
        } else {
            /* The human-readable job event log a bug report reads (SPEC section 7.3 / Task 9). */
            ava1_events_set_path(AVA1_DIR "/events.log");
            ava1_log_event("payload start: data layer up");
            /* Wall time only here: GC compares file mtimes, which are wall-clock. */
            rc = ava1_jobs_gc(AVA1_JOBS, (int64_t)time(NULL), 7 * 86400);
            if (rc < 0)
                /* A GC failure is logged and ignored, never returned: a stale jobs
                 * folder must not stop the payload from starting (the data layer is
                 * its own gate). Runs once, here, on the start path. */
                fprintf(stderr, "ava1: idle-job GC failed (%d); the data layer starts anyway\n", rc);
            else if (rc > 0)
                fprintf(stderr, "ava1: removed %d idle job directories\n", rc);
            if ((rc = ava1_data_start(&dc)) != 0)
                fprintf(stderr, "ava1: data layer did not start (%d); transfers are unavailable\n", rc);
            else {
                cfg.data = ava1_data_hooks();
                cfg.caps = AVA1_CAP_DATA_PLANE;
                g_data_on = 1;
                fsj_register_ops(); /* job.run: delete, chmod -R, hash, crc32 (the table adds the rest) */
            }
        }
    }
    /* The management methods are served when the management handlers' table is installed (main.c does it
     * before this): a client routes management by this bit, not by probing for ERR_UNKNOWN_METHOD. */
    if (mgmt_rpc_installed()) cfg.caps |= AVA1_CAP_MGMT;
    if (ava1_trust_slot_key(launcher) == 0) {
        /* Heap: ava1_peers_t is a few KB, more than this thread's stack should carry. */
        ava1_peers_t *peers = malloc(sizeof *peers);
        if (!peers) {
            on_log("ava1: launcher not trusted: out of memory");
        } else if (ava1_peers_load(peers, cfg.peers_path) != 0) {
            /* The server logs the unreadable file itself and keeps pairing closed. */
            on_log("ava1: launcher not trusted: the peers file could not be read");
        } else if (!ava1_peers_contains(peers, launcher) &&
                   ava1_peers_add(peers, launcher, "launcher", (uint64_t)time(NULL), cfg.peers_path) != 0) {
            on_log("ava1: launcher not trusted: cannot write " AVA1_DIR "/peers");
        }
        free(peers);
        /* A launch token (SPEC.md §5.2) lives in memory only: the server proves it to
         * the launcher in each Welcome, so the engine needs no pairing code either. */
        if (ava1_trust_slot_token(token) == 0) {
            cfg.has_launch = 1;
            memcpy(cfg.launch_key, launcher, 32);
            memcpy(cfg.launch_token, token, 16);
            crypto_wipe(token, sizeof token);
        }
    }
    {
        /* The server keeps its own copy: do not leave the private key on this stack. */
        int rc = ava1_server_start(&cfg);
        crypto_wipe(&cfg.identity, sizeof cfg.identity);
        crypto_wipe(cfg.launch_token, sizeof cfg.launch_token);
        return rc;
    }
}
