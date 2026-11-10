/* The helper's tailnet node: settings on /data, lifecycle, and the tailnet.* call.
 * The protocol, the data plane and the forwarder are tailnet/ (one C core shared with
 * the engine); this file only decides when it runs and with what. See tailnet_node.h. */
#include "tailnet_node.h"

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "atomic_file.h"
#include "json_escape.h"
#include "proc_identity.h"
#include "tailnet.h"
#include "webui_codec.h"

/* The allow-list. The core accepts nothing else from the tailnet. */
static const tn_serve SERVE[] = {
    {6, 9295, 0},  /* Remote Play session (TCP) */
    {17, 9296, 0}, /* Remote Play stream (UDP) */
    {17, 9302, 0}, /* Remote Play discovery / wake (UDP) */
    {6, 9120, 0},  /* the helper (AVA1, TCP) */
};

#define ALLOW_MAX 32

typedef struct {
    int enabled;
    char control_url[256];
    char hostname[64];
    char control_addr[64];
    /* Which tailnet devices may reach the PS5: 1 every one the tailnet allows (an explicit
     * opt-in), 0 only `allow` (the default: the devices the app added, and the ones the user
     * ticked). Settings saved before the list existed (12.0.0) read as listed with nobody on
     * it: each PS5Upload app adds itself when it next talks to the PS5 at home. The list is never
     * started from the whole tailnet. */
    int allow_all;
    uint32_t allow[ALLOW_MAX];
    int n_allow;
} tn_settings;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_dir[256] = TAILNET_NODE_DIR;
static tn_settings g_set;
static int g_loaded;
static tn_ctx *g_ctx;
/* The node's last error and warning, under their own lock: the node's thread sets them from
 * on_event, and stopping it (tn_destroy joins that thread) happens with g_mu held, so taking
 * g_mu in on_event would deadlock a stop that meets an error. Order: g_mu, then g_msg_mu. */
static pthread_mutex_t g_msg_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_last_error[160];
static char g_warning[200];
/* Devices the list turned away that the user chose to ignore (op blocked_ignore), under g_mu.
 * In memory only: after a restart a device that tries again is shown again. */
#define IGNORED_MAX TN_DENIED_MAX
static uint32_t g_ignored[IGNORED_MAX];
static int g_n_ignored, g_next_ignored;
/* The console is going to rest or has not settled after a wake (power_phase.c): the node keeps its
 * keys and settings but holds no sockets (tn_pause). Under g_mu; a node started meanwhile starts
 * paused. */
static int g_paused;
/* The core closed every socket by itself (TN_EV_NET_DOWN: the network going away, e.g. errno 163
 * as the console starts to rest) and not yet seen by power_phase (tailnet_node_take_down). Set from
 * the node's own threads in on_event: atomics only, never a lock that could wait. */
static atomic_int g_down_pending, g_down_errno;
/* power_phase has taken a down episode (so it is GOING_DOWN or later) and the node has not been
 * resumed since: only such an episode may be resumed by a set_paused(0) that changes nothing. */
static atomic_int g_down_taken;
static char g_down_what[160]; /* under g_msg_mu */

void tailnet_node_set_dir(const char *dir) {
    snprintf(g_dir, sizeof g_dir, "%s", dir);
    g_loaded = 0;
}

static void path_of(char *out, size_t cap, const char *name) { snprintf(out, cap, "%s/%s", g_dir, name); }

/* "100.64.0.1" -> host order, 0 when not an IPv4 in 100.64.0.0/10. */
static uint32_t parse_tailnet_ip(const char *s, size_t n) {
    uint32_t v = 0, part = 0;
    int dots = 0, digits = 0;
    for (size_t i = 0; i < n; i++) {
        char ch = s[i];
        if (ch >= '0' && ch <= '9') {
            part = part * 10 + (uint32_t)(ch - '0');
            if (++digits > 3 || part > 255) return 0;
        } else if (ch == '.') {
            if (!digits || ++dots > 3) return 0;
            v = v << 8 | part;
            part = 0;
            digits = 0;
        } else {
            return 0;
        }
    }
    if (dots != 3 || !digits) return 0;
    v = v << 8 | part;
    return (v >> 22) == (100u << 2 | 1u) ? v : 0; /* 100.64.0.0/10 */
}

/* A comma-separated list of tailnet IPs into the settings; -1 on a bad entry or too many. */
static int parse_allow(const char *list, tn_settings *s) {
    int n = 0;
    const char *p = list;
    while (*p) {
        while (*p == ' ' || *p == ',') p++;
        if (!*p) break;
        const char *e = p;
        while (*e && *e != ',' && *e != ' ') e++;
        uint32_t ip = parse_tailnet_ip(p, (size_t)(e - p));
        int dup = 0;
        for (int i = 0; i < n; i++) dup |= s->allow[i] == ip;
        if (!ip || (!dup && n >= ALLOW_MAX)) return -1;
        if (!dup) s->allow[n++] = ip;
        p = e;
    }
    s->n_allow = n;
    return 0;
}

static void push_allow_locked(void);

static void load_settings(void) {
    char path[320], buf[1024];
    if (g_loaded) return;
    g_loaded = 1;
    memset(&g_set, 0, sizeof g_set);
    snprintf(g_set.control_url, sizeof g_set.control_url, "%s", TAILNET_DEFAULT_CONTROL);
    path_of(path, sizeof path, "settings.json");
    FILE *f = fopen(path, "r");
    if (!f) return; /* never set up: the list starts empty (allow_all 0) */
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    int b = 0;
    if (webui_json_bool(buf, n, "enabled", &b) == 1) g_set.enabled = b;
    char url[256];
    if (webui_json_str(buf, n, "control_url", url, sizeof url) == 1 && url[0]) snprintf(g_set.control_url, sizeof g_set.control_url, "%s", url);
    (void)webui_json_str(buf, n, "hostname", g_set.hostname, sizeof g_set.hostname);
    (void)webui_json_str(buf, n, "control_addr", g_set.control_addr, sizeof g_set.control_addr);
    char mode[16], list[ALLOW_MAX * 16 + 8];
    /* No allow_mode (12.0.0 settings): listed, nobody yet. */
    if (webui_json_str(buf, n, "allow_mode", mode, sizeof mode) == 1) g_set.allow_all = !strcmp(mode, "all");
    if (webui_json_str(buf, n, "allow", list, sizeof list) == 1) {
        tn_settings tmp = g_set;
        if (parse_allow(list, &tmp) == 0) g_set = tmp;
    }
}

/* "a.b.c.d,…" of the saved list. */
static void allow_str(char *out, size_t cap) {
    size_t len = 0;
    char ip[16];
    out[0] = 0;
    for (int i = 0; i < g_set.n_allow; i++) {
        int k = snprintf(out + len, cap - len, "%s%s", i ? "," : "", tn_ip4_str(g_set.allow[i], ip));
        if (k < 0 || (size_t)k >= cap - len) break;
        len += (size_t)k;
    }
}

static int save_settings(void) {
    char path[320], tmp[330], url[300], host[100], addr[80], list[ALLOW_MAX * 16 + 8], buf[2048];
    mkdir(g_dir, 0700);
    path_of(path, sizeof path, "settings.json");
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    json_escape_into(g_set.control_url, url, sizeof url);
    json_escape_into(g_set.hostname, host, sizeof host);
    json_escape_into(g_set.control_addr, addr, sizeof addr);
    allow_str(list, sizeof list);
    int n = snprintf(buf, sizeof buf,
                     "{\"enabled\":%s,\"control_url\":\"%s\",\"hostname\":\"%s\",\"control_addr\":\"%s\","
                     "\"allow_mode\":\"%s\",\"allow\":\"%s\"}\n",
                     g_set.enabled ? "true" : "false", url, host, addr, g_set.allow_all ? "all" : "listed", list);
    if (n <= 0 || (size_t)n >= sizeof buf) return -1;
    return atomic_file_write(path, tmp, buf, (size_t)n, 0600);
}

static void on_thread(void *user, const char *role) {
    (void)user;
    (void)role;
    proc_name_set_self("ps5upload-tailnet"); /* every helper thread takes our process name */
}

/* The node's lines in stderr.log, at most LOG_BURST per minute (a failing network must not fill
 * /data); what was dropped is counted on the next line that gets through. Called from the node's
 * two threads. */
#define LOG_BURST 40
static pthread_mutex_t g_log_mu = PTHREAD_MUTEX_INITIALIZER;
static int64_t g_log_window_s;
static unsigned g_log_n, g_log_dropped;

static int log_allowed(void) {
    struct timespec ts;
    int64_t now = clock_gettime(CLOCK_MONOTONIC, &ts) == 0 ? (int64_t)ts.tv_sec : 0;
    int ok;
    unsigned dropped = 0;
    pthread_mutex_lock(&g_log_mu);
    if (now - g_log_window_s >= 60 || now < g_log_window_s) {
        g_log_window_s = now;
        g_log_n = 0;
        dropped = g_log_dropped;
        g_log_dropped = 0;
    }
    ok = g_log_n < LOG_BURST;
    if (ok) g_log_n++;
    else g_log_dropped++;
    pthread_mutex_unlock(&g_log_mu);
    if (dropped) fprintf(stderr, "[tailnet] (%u lines not logged in the last minute)\n", dropped);
    return ok;
}

static void on_event(void *user, const tn_event *ev) {
    (void)user;
    /* Every line but state changes, sign-in and the network going away or coming back goes through
     * the limit; those are rare (the last two once per episode), and a dropped going-away line would
     * also drop power_phase's signal. */
    if (ev->type != TN_EV_STATE && ev->type != TN_EV_LOGIN_URL && ev->type != TN_EV_NETMAP &&
        ev->type != TN_EV_NET_DOWN && ev->type != TN_EV_NET_UP && !log_allowed()) {
        if (ev->type == TN_EV_ERROR || ev->type == TN_EV_WARN) {
            /* still remembered for the app's status, just not written */
            pthread_mutex_lock(&g_msg_mu);
            snprintf(ev->type == TN_EV_ERROR ? g_last_error : g_warning,
                     ev->type == TN_EV_ERROR ? sizeof g_last_error : sizeof g_warning, "%s", ev->text ? ev->text : "");
            pthread_mutex_unlock(&g_msg_mu);
        }
        return;
    }
    switch (ev->type) {
    case TN_EV_STATE:
        fprintf(stderr, "[tailnet] state: %s\n", ev->text);
        if (ev->state == TN_ST_RUNNING) {
            /* In: the error that kept it out is over (the app shows `error` while it is set). */
            pthread_mutex_lock(&g_msg_mu);
            g_last_error[0] = 0;
            pthread_mutex_unlock(&g_msg_mu);
        }
        break;
    case TN_EV_LOGIN_URL:
        fprintf(stderr, "[tailnet] waiting for sign-in\n"); /* the URL itself goes to the app only */
        break;
    case TN_EV_ERROR:
        pthread_mutex_lock(&g_msg_mu);
        snprintf(g_last_error, sizeof g_last_error, "%s", ev->text ? ev->text : "");
        pthread_mutex_unlock(&g_msg_mu);
        fprintf(stderr, "[tailnet] %s\n", ev->text ? ev->text : "error");
        break;
    case TN_EV_WARN:
        pthread_mutex_lock(&g_msg_mu);
        snprintf(g_warning, sizeof g_warning, "%s", ev->text ? ev->text : "");
        pthread_mutex_unlock(&g_msg_mu);
        fprintf(stderr, "[tailnet] %s\n", ev->text ? ev->text : "");
        break;
    case TN_EV_PATH:
        fprintf(stderr, "[tailnet] %s\n", ev->text);
        break;
    case TN_EV_NET_DOWN:
        /* Its sockets are already closed; power_phase takes it as a going-down signal on its next
         * poll (pauses the node, and resumes it only once the console has settled). */
        pthread_mutex_lock(&g_msg_mu);
        snprintf(g_down_what, sizeof g_down_what, "%s", ev->text ? ev->text : "");
        pthread_mutex_unlock(&g_msg_mu);
        atomic_store(&g_down_errno, ev->err);
        atomic_store(&g_down_pending, 1);
        fprintf(stderr, "[tailnet] %s\n", ev->text ? ev->text : "network going away");
        break;
    case TN_EV_NET_UP:
        fprintf(stderr, "[tailnet] network back: %s\n", ev->text ? ev->text : "");
        break;
    case TN_EV_LOG:
        /* The core's own lines at the level it was given (0: the ones that explain a failure,
         * e.g. "clock jumped +3600 s: re-establishing paths", "map poll: no message for 120 s"). */
        fprintf(stderr, "[tailnet] %s\n", ev->text ? ev->text : "");
        break;
    default:
        break;
    }
}

/* Start with the current settings; caller holds g_mu. */
static int start_locked(const char *authkey) {
    if (g_ctx) return 0;
    char dir[256];
    snprintf(dir, sizeof dir, "%s", g_dir);
    mkdir(dir, 0700);
    tn_config cfg;
    memset(&cfg, 0, sizeof cfg);
    cfg.state_dir = dir;
    cfg.control_url = g_set.control_url;
    cfg.control_addr = g_set.control_addr[0] ? g_set.control_addr : NULL;
    cfg.auth_key = authkey && *authkey ? authkey : NULL;
    cfg.hostname = g_set.hostname[0] ? g_set.hostname : "PS5";
    cfg.os = "ps5";
    cfg.app = "ps5upload";
    cfg.cb = on_event;
    cfg.serve = SERVE;
    cfg.n_serve = sizeof SERVE / sizeof SERVE[0];
    cfg.thread_cb = on_thread;
    cfg.thread_stack = 256 * 1024;
    /* A console needs far less than the desktop defaults (6.4 MiB): a home tailnet. */
    cfg.max_peers = 64;
    cfg.max_regions = 64;
    cfg.max_rules = 64;
    cfg.map_msg_max = 1u << 20;
    cfg.json_tokens_max = 16384;
    tn_ctx *c = NULL;
    int r = tn_create(&cfg, &c);
    if (r == 0) r = tn_start(c);
    if (r != 0) {
        pthread_mutex_lock(&g_msg_mu);
        snprintf(g_last_error, sizeof g_last_error, "%s", tn_strerror(r));
        pthread_mutex_unlock(&g_msg_mu);
        fprintf(stderr, "[tailnet] could not start: %s\n", tn_strerror(r));
        if (c) tn_destroy(c);
        return -1;
    }
    fprintf(stderr, "[tailnet] started (%zu KiB)%s\n", tn_memory_footprint(c) / 1024,
            g_paused ? ", paused until the console has settled" : "");
    g_ctx = c;
    /* The console's rest/wake tracking (power_phase) decides when it comes back, never the core on
     * its own; two control errors in a row also count as the network going away (what the PS5
     * showed in the seconds before it froze for a rest it did not announce). */
    tn_set_net_policy(c, TN_NETPOL_CONTROL_STREAK);
    if (g_paused) tn_pause(c);
    push_allow_locked();
    pthread_mutex_lock(&g_msg_mu);
    g_last_error[0] = 0;
    pthread_mutex_unlock(&g_msg_mu);
    return 0;
}

/* The saved list into the running node; caller holds g_mu. */
static void push_allow_locked(void) {
    if (!g_ctx) return;
    (void)tn_set_peer_allow(g_ctx, g_set.allow, g_set.allow_all ? -1 : g_set.n_allow);
}

int tailnet_node_set_paused(int paused) {
    /* Never wait here: the caller is the wake watchdog, and a start or stop holding g_mu can take a
     * while (tn_destroy joins the node's threads). It asks again on its next poll. */
    if (pthread_mutex_trylock(&g_mu) != 0) return -1;
    paused = paused ? 1 : 0;
    if (paused != g_paused) {
        g_paused = paused;
        if (g_ctx) {
            if (paused) {
                tn_pause(g_ctx);
            } else {
                tn_resume(g_ctx); /* also ends a down episode of the core */
                atomic_store(&g_down_taken, 0);
            }
            fprintf(stderr, "[tailnet] %s\n",
                    paused ? "paused: the console is going to rest or still waking (no sockets until it settles)"
                           : "resumed: rejoining the tailnet");
        }
    } else if (!paused && g_ctx && tn_net_down(g_ctx) && atomic_load(&g_down_taken)) {
        /* Down by itself, the episode taken by power_phase, which now says the console has settled,
         * but its pause never landed (the lock was busy then): resume here, the one path. A down
         * episode power_phase has not taken yet is never resumed. */
        atomic_store(&g_down_taken, 0);
        tn_resume(g_ctx);
        fprintf(stderr, "[tailnet] resumed after closing its sockets: the console has settled\n");
    }
    pthread_mutex_unlock(&g_mu);
    return 0;
}

int tailnet_node_take_down(int *err, char *what, size_t cap) {
    if (!atomic_exchange(&g_down_pending, 0)) return 0;
    atomic_store(&g_down_taken, 1);
    if (err) *err = atomic_load(&g_down_errno);
    if (what && cap) {
        pthread_mutex_lock(&g_msg_mu);
        snprintf(what, cap, "%s", g_down_what);
        pthread_mutex_unlock(&g_msg_mu);
    }
    return 1;
}

void tailnet_node_test_net_down(int err) {
    pthread_mutex_lock(&g_mu);
    if (g_ctx) tn_test_net_down(g_ctx, err);
    pthread_mutex_unlock(&g_mu);
}

int tailnet_node_net_down(void) {
    pthread_mutex_lock(&g_mu);
    int d = g_ctx ? tn_net_down(g_ctx) : 0;
    pthread_mutex_unlock(&g_mu);
    return d;
}

const char *tailnet_node_brief(void) {
    /* For a log line from the wake watchdog: never waits, not on a start or stop (g_mu) and not on
     * the data plane (tn_get_status takes its lock). */
    if (pthread_mutex_trylock(&g_mu) != 0) return "busy";
    const char *r = !g_ctx ? (g_paused ? "off,paused" : "off")
                    : tn_net_down(g_ctx) ? (g_paused ? "on,paused,closed" : "on,closed")
                    : g_paused ? "on,paused" : "on";
    pthread_mutex_unlock(&g_mu);
    return r;
}

int tailnet_node_paused(void) {
    pthread_mutex_lock(&g_mu);
    int p = g_paused;
    pthread_mutex_unlock(&g_mu);
    return p;
}

void tailnet_node_test_rest_wake(void) {
    pthread_mutex_lock(&g_mu);
    if (g_ctx) tn_test_rewake(g_ctx);
    pthread_mutex_unlock(&g_mu);
}

void tailnet_node_test_denied(unsigned int ip, unsigned int port) {
    pthread_mutex_lock(&g_mu);
    if (g_ctx) tn_test_peer_denied(g_ctx, ip, 6, (uint16_t)port);
    pthread_mutex_unlock(&g_mu);
}

static int ignored_locked(uint32_t ip) {
    for (int i = 0; i < g_n_ignored; i++)
        if (g_ignored[i] == ip) return 1;
    return 0;
}

static void ignore_locked(uint32_t ip) {
    if (ignored_locked(ip)) return;
    g_ignored[g_next_ignored] = ip;
    g_next_ignored = (g_next_ignored + 1) % IGNORED_MAX;
    if (g_n_ignored < IGNORED_MAX) g_n_ignored++;
}

int tailnet_node_allows(unsigned int ip) {
    pthread_mutex_lock(&g_mu);
    load_settings();
    int ok = g_set.allow_all;
    for (int i = 0; i < g_set.n_allow && !ok; i++) ok = g_set.allow[i] == ip;
    pthread_mutex_unlock(&g_mu);
    return ok;
}

static void stop_locked(void) {
    if (!g_ctx) return;
    tn_ctx *c = g_ctx;
    g_ctx = NULL;
    tn_destroy(c);
}

int tailnet_node_boot(void) {
    pthread_mutex_lock(&g_mu);
    load_settings();
    int r = g_set.enabled ? start_locked(NULL) : 0;
    pthread_mutex_unlock(&g_mu);
    return r;
}

void tailnet_node_shutdown(void) {
    pthread_mutex_lock(&g_mu);
    stop_locked();
    pthread_mutex_unlock(&g_mu);
}

static int put(char *out, size_t cap, size_t *len, const char *fmt, const char *s1) {
    int n = snprintf(out + *len, cap - *len, fmt, s1);
    if (n < 0 || (size_t)n >= cap - *len) return -1;
    *len += (size_t)n;
    return 0;
}

/* {"ok":true,"enabled":…,"state":"running","login_url":…,"control_url":…,"hostname":…,
 *  "ip":"100.x.y.z","name":…,"home_derp":900,"derp_connected":true,"home_latency_ms":1.2,
 *  "udp_port":41641,"endpoints":["a.b.c.d:p",…],"key_expiry":unix|0,"warning":…,"error":…,
 *  "serving":{"udp_flows":n,"tcp_conns":n},"peers":[{"name","ip","online","path","direct",
 *  "derp_region","latency_ms","last_rx_ms"}…],"blocked_recent":[{"ip","name","port","proto",
 *  "count","first","last"}…]} — caller holds g_mu. */
static int status_json(char *out, size_t cap) {
    tn_status st;
    static tn_peer_status ps[64];
    char e1[600], e2[300], ip[16];
    size_t len = 0;
    memset(&st, 0, sizeof st);
    if (g_ctx) tn_get_status(g_ctx, &st, ps, 64);
    if (put(out, cap, &len, "{\"ok\":true,\"enabled\":%s", g_set.enabled ? "true" : "false") < 0) return -1;
    if (put(out, cap, &len, ",\"state\":\"%s\"", g_ctx ? tn_state_name(st.state) : "off") < 0) return -1;
    json_escape_into(st.login_url, e1, sizeof e1);
    if (put(out, cap, &len, ",\"login_url\":\"%s\"", e1) < 0) return -1;
    json_escape_into(g_set.control_url, e1, sizeof e1);
    if (put(out, cap, &len, ",\"control_url\":\"%s\"", e1) < 0) return -1;
    json_escape_into(g_set.hostname, e2, sizeof e2);
    if (put(out, cap, &len, ",\"hostname\":\"%s\"", e2) < 0) return -1;
    {
        char path[320], list[ALLOW_MAX * 16 + 8];
        path_of(path, sizeof path, "tailnet-state.json");
        if (put(out, cap, &len, ",\"signed_in\":%s", access(path, F_OK) == 0 ? "true" : "false") < 0) return -1;
        if (put(out, cap, &len, ",\"allow_mode\":\"%s\"", g_set.allow_all ? "all" : "listed") < 0) return -1;
        allow_str(list, sizeof list);
        if (put(out, cap, &len, ",\"allow\":\"%s\"", list) < 0) return -1;
    }
    if (put(out, cap, &len, ",\"ip\":\"%s\"", st.self_addr4 ? tn_ip4_str(st.self_addr4, ip) : "") < 0) return -1;
    json_escape_into(st.self_name, e2, sizeof e2);
    if (put(out, cap, &len, ",\"name\":\"%s\"", e2) < 0) return -1;
    int n = snprintf(out + len, cap - len,
                     ",\"home_derp\":%d,\"derp_connected\":%s,\"home_latency_ms\":%.2f,\"udp_port\":%u,\"key_expiry\":%lld"
                     ",\"serving\":{\"udp_flows\":%u,\"tcp_conns\":%u},\"endpoints\":[",
                     (int)st.home_derp, st.derp_connected ? "true" : "false",
                     st.home_latency_us >= 0 ? st.home_latency_us / 1000.0 : -1.0, (unsigned)st.udp_port,
                     (long long)st.key_expiry, st.fwd_udp_flows, st.fwd_tcp_conns);
    if (n < 0 || (size_t)n >= cap - len) return -1;
    len += (size_t)n;
    for (uint32_t i = 0; i < st.n_endpoints && i < 8; i++) {
        n = snprintf(out + len, cap - len, "%s\"%s:%u\"", i ? "," : "", tn_ip4_str(st.endpoints[i].ip, ip),
                     (unsigned)st.endpoints[i].port);
        if (n < 0 || (size_t)n >= cap - len) return -1;
        len += (size_t)n;
    }
    char e3[sizeof e2];
    pthread_mutex_lock(&g_msg_mu);
    json_escape_into(g_warning, e2, sizeof e2);
    json_escape_into(g_last_error, e3, sizeof e3);
    pthread_mutex_unlock(&g_msg_mu);
    if (put(out, cap, &len, "],\"warning\":\"%s\"", e2) < 0) return -1;
    memcpy(e2, e3, sizeof e2);
    if (put(out, cap, &len, ",\"error\":\"%s\",\"peers\":[", e2) < 0) return -1;
    for (uint32_t i = 0; i < st.n_peers && i < 64; i++) {
        const tn_peer_status *p = &ps[i];
        char dir[24] = "";
        if (p->path == TN_PATH_DIRECT)
            snprintf(dir, sizeof dir, "%s:%u", tn_ip4_str(p->direct.ip, ip), (unsigned)p->direct.port);
        json_escape_into(p->name, e2, sizeof e2);
        n = snprintf(out + len, cap - len,
                     "%s{\"name\":\"%s\",\"ip\":\"%s\",\"online\":%s,\"path\":\"%s\",\"direct\":\"%s\",\"derp_region\":%d,"
                     "\"latency_ms\":%.2f,\"last_rx_ms\":%lld}",
                     i ? "," : "", e2, tn_ip4_str(p->addr4, ip), p->online ? "true" : "false",
                     p->path == TN_PATH_DIRECT ? "direct" : p->path == TN_PATH_DERP ? "relayed" : "idle", dir,
                     (int)p->derp_region, p->latency_us >= 0 ? p->latency_us / 1000.0 : -1.0, (long long)p->last_rx_ms);
        if (n < 0 || (size_t)n >= cap - len) return -1;
        len += (size_t)n;
    }
    /* "blocked_recent": devices the list turned away (newest first), named from the peer list,
     * without the ones the user chose to ignore. Only while listing devices: with every device
     * let in there is nothing to turn away. */
    if (put(out, cap, &len, "],\"blocked_recent\":[%s", "") < 0) return -1;
    if (g_ctx && !g_set.allow_all) {
        tn_denied den[TN_DENIED_MAX];
        int nd = tn_peer_denied(g_ctx, den, TN_DENIED_MAX), shown = 0;
        for (int i = 0; i < nd; i++) {
            if (ignored_locked(den[i].ip)) continue;
            const char *name = "";
            for (uint32_t j = 0; j < st.n_peers && j < 64; j++)
                if (ps[j].addr4 == den[i].ip) name = ps[j].name;
            json_escape_into(name, e2, sizeof e2);
            n = snprintf(out + len, cap - len,
                         "%s{\"ip\":\"%s\",\"name\":\"%s\",\"port\":%u,\"proto\":\"%s\",\"count\":%u,\"first\":%lld,\"last\":%lld}",
                         shown ? "," : "", tn_ip4_str(den[i].ip, ip), e2, (unsigned)den[i].port,
                         den[i].proto == 17 ? "udp" : "tcp", (unsigned)den[i].count, (long long)den[i].first_s,
                         (long long)den[i].last_s);
            if (n < 0 || (size_t)n >= cap - len) return -1;
            len += (size_t)n;
            shown++;
        }
    }
    if (put(out, cap, &len, "]}%s", "") < 0) return -1;
    return (int)len;
}

static int err_json(char *out, size_t cap, const char *err) {
    int n = snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", err);
    return n > 0 && (size_t)n < cap ? n : -1;
}

int tailnet_node_call(const char *body, size_t len, char *out, size_t cap) {
    char op[20] = "status", url[256] = "", host[64] = "", key[256] = "", addr[64] = "";
    if (len > 4096) return err_json(out, cap, "bad_request");
    if (len > 0 && webui_json_str(body, len, "op", op, sizeof op) < 0) return err_json(out, cap, "bad_request");
    pthread_mutex_lock(&g_mu);
    load_settings();
    int r = 0;
    if (!strcmp(op, "enable")) {
        if (webui_json_str(body, len, "control_url", url, sizeof url) < 0 ||
            webui_json_str(body, len, "hostname", host, sizeof host) < 0 ||
            webui_json_str(body, len, "authkey", key, sizeof key) < 0 ||
            webui_json_str(body, len, "control_addr", addr, sizeof addr) < 0 ||
            (url[0] && strncmp(url, "https://", 8) && strncmp(url, "http://", 7))) {
            pthread_mutex_unlock(&g_mu);
            return err_json(out, cap, "bad_request");
        }
        int changed = 0;
        if (url[0] && strcmp(url, g_set.control_url)) {
            snprintf(g_set.control_url, sizeof g_set.control_url, "%s", url);
            changed = 1;
        }
        if (host[0] && strcmp(host, g_set.hostname)) {
            snprintf(g_set.hostname, sizeof g_set.hostname, "%s", host);
            changed = 1;
        }
        if (addr[0]) snprintf(g_set.control_addr, sizeof g_set.control_addr, "%s", addr);
        g_set.enabled = 1;
        if (save_settings() != 0) fprintf(stderr, "[tailnet] settings not saved (errno %d)\n", errno);
        if (changed || key[0]) stop_locked(); /* new server, name or key: a fresh start */
        r = start_locked(key);
        memset(key, 0, sizeof key);
    } else if (!strcmp(op, "disable") || !strcmp(op, "logout")) {
        stop_locked();
        g_set.enabled = 0;
        if (save_settings() != 0) fprintf(stderr, "[tailnet] settings not saved (errno %d)\n", errno);
        if (!strcmp(op, "logout")) {
            char path[320];
            path_of(path, sizeof path, "tailnet-state.json"); /* the node and machine keys */
            if (unlink(path) != 0 && errno != ENOENT) r = -1;
            pthread_mutex_lock(&g_msg_mu);
            g_warning[0] = 0;
            pthread_mutex_unlock(&g_msg_mu);
        }
    } else if (!strcmp(op, "rename")) {
        /* The device name, any time: kept while off, applied now while on (the node starts
         * again under it; control keeps its address). */
        if (webui_json_str(body, len, "hostname", host, sizeof host) != 1 || !host[0]) {
            pthread_mutex_unlock(&g_mu);
            return err_json(out, cap, "bad_request");
        }
        if (strcmp(host, g_set.hostname)) {
            snprintf(g_set.hostname, sizeof g_set.hostname, "%s", host);
            if (save_settings() != 0) fprintf(stderr, "[tailnet] settings not saved (errno %d)\n", errno);
            if (g_ctx) {
                stop_locked();
                r = start_locked(NULL);
            }
        }
    } else if (!strcmp(op, "renew")) {
        /* Same node, a new sign-in: a status shortly after carries the login URL. */
        if (!g_ctx) {
            pthread_mutex_unlock(&g_mu);
            return err_json(out, cap, "not_running");
        }
        r = tn_reauth(g_ctx) == 0 ? 0 : -1;
    } else if (!strcmp(op, "allow") || !strcmp(op, "allow_add")) {
        char mode[16] = "", list[ALLOW_MAX * 16 + 8] = "";
        tn_settings next = g_set;
        if (!strcmp(op, "allow")) {
            if (webui_json_str(body, len, "allow_mode", mode, sizeof mode) < 0 ||
                webui_json_str(body, len, "allow", list, sizeof list) < 0 ||
                (mode[0] && strcmp(mode, "all") && strcmp(mode, "listed")) || parse_allow(list, &next) != 0) {
                pthread_mutex_unlock(&g_mu);
                return err_json(out, cap, "bad_request");
            }
            if (mode[0]) next.allow_all = !strcmp(mode, "all");
        } else {
            uint32_t ip = 0;
            if (webui_json_str(body, len, "ip", list, sizeof list) == 1) ip = parse_tailnet_ip(list, strlen(list));
            if (!ip) {
                pthread_mutex_unlock(&g_mu);
                return err_json(out, cap, "bad_request");
            }
            int have = 0;
            for (int i = 0; i < next.n_allow; i++) have |= next.allow[i] == ip;
            if (!have) {
                if (next.n_allow >= ALLOW_MAX) {
                    pthread_mutex_unlock(&g_mu);
                    return err_json(out, cap, "too_many");
                }
                next.allow[next.n_allow++] = ip;
            }
        }
        g_set = next;
        if (save_settings() != 0) fprintf(stderr, "[tailnet] settings not saved (errno %d)\n", errno);
        push_allow_locked();
    } else if (!strcmp(op, "blocked_ignore")) {
        /* "Ignore" on "<device> tried to reach this PS5": stop showing it (until a restart). */
        char ipbuf[24] = "";
        uint32_t ip = 0;
        if (webui_json_str(body, len, "ip", ipbuf, sizeof ipbuf) == 1) ip = parse_tailnet_ip(ipbuf, strlen(ipbuf));
        if (!ip) {
            pthread_mutex_unlock(&g_mu);
            return err_json(out, cap, "bad_request");
        }
        ignore_locked(ip);
    } else if (strcmp(op, "status") != 0) {
        pthread_mutex_unlock(&g_mu);
        return err_json(out, cap, "unknown_op");
    }
    int n = r == 0 ? status_json(out, cap) : -1;
    pthread_mutex_unlock(&g_mu);
    if (n < 0) return err_json(out, cap, r ? "start_failed" : "too_big");
    return n;
}
