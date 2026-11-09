/* See include/mesh.h. */
#include "mesh.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "webui_codec.h"

#define KIND_MAX 16
#define OS_MAX 48
#define APP_MAX 24
#define NAME_MAX_ 64
#define LOCK_NAME_MAX 160
#define LABEL_MAX 120
#define EVENT_TEXT_MAX 160
#define LOCK_TTL_DEFAULT_S 60u
#define LOCK_TTL_MAX_S 600u
#define PRUNE_EVERY_MS (3600u * 1000u)
#define VIA_MAX 12
/* A device that asked to connect while approval is on: listed for the owner to Allow or Decline
 * for this long. Allowed, it has the same time to connect; declined, it is not asked about again
 * until then. */
#define ASK_TTL_MS (10u * 60u * 1000u)

typedef struct {
    int used;
    uint8_t key[32];
    uint64_t first, last; /* ms */
    uint32_t ip;
    int blocked;
    char kind[KIND_MAX], os[OS_MAX], app[APP_MAX], name[NAME_MAX_], nick[NAME_MAX_];
    char activity[MESH_ACTIVITY_MAX + 1]; /* "" = none */
    uint64_t activity_at;
    /* How it was let in: "network" (approval off, on the home network), "tailscale" (approval off,
     * over Tailscale), "approved" (asked, and the owner allowed it); "" when not known (paired with
     * a code, launched by its app, or listed before 12.2). Kept in v1/via, so an older helper reading
     * v1/devices sees the file it knows. */
    char via[VIA_MAX];
} dev_t_;

enum { ASK_ASKING = 0, ASK_ALLOWED = 1, ASK_DECLINED = 2 };

typedef struct {
    int used, state;
    uint8_t key[32];
    char name[NAME_MAX_];
    uint32_t ip;
    uint64_t at;      /* wall ms, for showing */
    uint64_t at_mono; /* ms, for expiry */
} ask_t_;

#define OWNER_MAX 40

typedef struct {
    int used;
    char name[LOCK_NAME_MAX], label[LABEL_MAX];
    uint8_t by[32];
    /* Which window of that device (a browser of a Docker engine, say): two windows of one device
     * are two holders. "" for a caller that does not say. */
    char owner[OWNER_MAX];
    /* Expiry on the monotonic clock: the console's wall clock jumps (a date set, NTP), and a lock
     * must neither die early nor outlive its holder because of that. */
    uint64_t until_mono; /* ms */
} lock_t_;

static int holds(const lock_t_ *l, const uint8_t caller[32], const char *owner) {
    return memcmp(l->by, caller, 32) == 0 && strcmp(l->owner, owner) == 0;
}

typedef struct {
    uint64_t seq, at;
    char kind[24];
    uint8_t by[32];
    char text[EVENT_TEXT_MAX];
} event_t_;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
    char dir[256];
    int persist;
    mesh_hooks_t h;
    char helper[32];
    dev_t_ dev[MESH_MAX_DEVICES];
    lock_t_ locks[MESH_LOCKS_MAX];
    event_t_ ev[MESH_EVENTS_MAX];
    uint64_t ev_seq;
    uint64_t rev;
    uint64_t online_sig;
    uint64_t last_prune;
    int require_approval;
    unsigned prune_days;
    ask_t_ asks[MESH_ASKS_MAX];
} M;

/* ---- small helpers ---- */

static uint64_t now_ms(void) { return M.h.now_ms ? M.h.now_ms() : 0; }

static uint64_t mono_ms(void) {
    struct timespec ts;
    if (M.h.mono_ms) return M.h.mono_ms();
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

/* A lock's expiry as wall time (what callers show), from the wall time `t` taken now. */
static uint64_t lock_until_wall(const lock_t_ *l, uint64_t t) {
    uint64_t m = mono_ms();
    return l->until_mono > m ? t + (l->until_mono - m) : t;
}

uint64_t mesh_wall_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void to_hex(const uint8_t key[32], char out[65]) {
    static const char d[] = "0123456789abcdef";
    int i;
    for (i = 0; i < 32; i++) {
        out[2 * i] = d[key[i] >> 4];
        out[2 * i + 1] = d[key[i] & 15];
    }
    out[64] = 0;
}

static int hexval(int c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int from_hex(const char *s, uint8_t key[32]) {
    int i;
    if (strlen(s) != 64) return -1;
    for (i = 0; i < 32; i++) {
        int a = hexval((unsigned char)s[2 * i]), b = hexval((unsigned char)s[2 * i + 1]);
        if (a < 0 || b < 0) return -1;
        key[i] = (uint8_t)(a << 4 | b);
    }
    return 0;
}

/* A field as stored: no tab, newline or other control byte, cut to cap - 1 bytes on a UTF-8 boundary. */
static void clean_field(char *out, size_t cap, const char *in) {
    size_t j = 0;
    if (!in) in = "";
    if (cap == 0) return;
    while (in[j] && j + 1 < cap) {
        unsigned char c = (unsigned char)in[j];
        out[j++] = c < 0x20 || c == 0x7f ? ' ' : (char)c;
    }
    /* Cut inside a character: drop its first bytes too, never leave half of one behind. */
    if (in[j] && ((unsigned char)in[j] & 0xC0u) == 0x80u) {
        while (j > 0 && ((unsigned char)out[j - 1] & 0xC0u) == 0x80u) j--;
        if (j > 0) j--;
    }
    out[j] = 0;
}

typedef struct {
    char *p;
    size_t len, cap;
    int err;
} buf_t;

static void put(buf_t *b, const char *s, size_t n) {
    if (b->err) return;
    if (b->len + n + 1 > b->cap) {
        b->err = 1;
        return;
    }
    memcpy(b->p + b->len, s, n);
    b->len += n;
    b->p[b->len] = 0;
}
static void puts_(buf_t *b, const char *s) { put(b, s, strlen(s)); }
static void putf(buf_t *b, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void putf(buf_t *b, const char *fmt, ...) {
    va_list ap;
    int n;
    if (b->err) return;
    va_start(ap, fmt);
    n = vsnprintf(b->p + b->len, b->cap - b->len, fmt, ap);
    va_end(ap);
    if (n < 0 || (size_t)n >= b->cap - b->len) {
        b->err = 1;
        return;
    }
    b->len += (size_t)n;
}
static void putstr(buf_t *b, const char *s) {
    size_t need;
    if (b->err) return;
    need = webui_json_escape(NULL, 0, s, strlen(s));
    if (b->len + need + 3 > b->cap) {
        b->err = 1;
        return;
    }
    b->p[b->len++] = '"';
    webui_json_escape(b->p + b->len, b->cap - b->len, s, strlen(s));
    b->len += need;
    b->p[b->len++] = '"';
    b->p[b->len] = 0;
}

static int fail(char *out, size_t cap, const char *token) {
    int n = snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", token);
    return n < 0 || (size_t)n >= cap ? -1 : n;
}

static void ip_text(uint32_t ip, char out[16]) {
    const uint8_t *b = (const uint8_t *)&ip;
    snprintf(out, 16, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]);
}

/* ---- JSON value spans (only what this module needs: a top-level key's object or array) ---- */

static void skip_ws(const char *s, size_t n, size_t *i) {
    while (*i < n && (s[*i] == ' ' || s[*i] == '\t' || s[*i] == '\n' || s[*i] == '\r')) (*i)++;
}

static int skip_string(const char *s, size_t n, size_t *i) {
    if (*i >= n || s[*i] != '"') return -1;
    for ((*i)++; *i < n; (*i)++) {
        if (s[*i] == '\\') {
            (*i)++;
            continue;
        }
        if (s[*i] == '"') {
            (*i)++;
            return 0;
        }
        if ((unsigned char)s[*i] < 0x20) return -1;
    }
    return -1;
}

static int lit(const char *s, size_t n, size_t *i, const char *w) {
    size_t l = strlen(w);
    if (n - *i < l || memcmp(s + *i, w, l) != 0) return -1;
    *i += l;
    return 0;
}

static int digits(const char *s, size_t n, size_t *i) {
    size_t st = *i;
    while (*i < n && s[*i] >= '0' && s[*i] <= '9') (*i)++;
    return *i > st ? 0 : -1;
}

/* Moves past one value, checking its syntax. Nesting is capped (depth 32), so the recursion is
 * bounded whatever the body. */
static int skip_value_d(const char *s, size_t n, size_t *i, int depth) {
    skip_ws(s, n, i);
    if (*i >= n || depth > 32) return -1;
    switch (s[*i]) {
    case '"': return skip_string(s, n, i);
    case 't': return lit(s, n, i, "true");
    case 'f': return lit(s, n, i, "false");
    case 'n': return lit(s, n, i, "null");
    case '{':
    case '[': {
        char close = s[*i] == '{' ? '}' : ']';
        int obj = close == '}';
        (*i)++;
        skip_ws(s, n, i);
        if (*i < n && s[*i] == close) {
            (*i)++;
            return 0;
        }
        for (;;) {
            if (obj) {
                skip_ws(s, n, i);
                if (skip_string(s, n, i) != 0) return -1;
                skip_ws(s, n, i);
                if (*i >= n || s[*i] != ':') return -1;
                (*i)++;
            }
            if (skip_value_d(s, n, i, depth + 1) != 0) return -1;
            skip_ws(s, n, i);
            if (*i >= n) return -1;
            if (s[*i] == ',') {
                (*i)++;
                continue;
            }
            if (s[*i] != close) return -1;
            (*i)++;
            return 0;
        }
    }
    default:
        if (s[*i] == '-') (*i)++;
        if (digits(s, n, i) != 0) return -1;
        if (*i < n && s[*i] == '.') {
            (*i)++;
            if (digits(s, n, i) != 0) return -1;
        }
        if (*i < n && (s[*i] == 'e' || s[*i] == 'E')) {
            (*i)++;
            if (*i < n && (s[*i] == '+' || s[*i] == '-')) (*i)++;
            if (digits(s, n, i) != 0) return -1;
        }
        return 0;
    }
}

static int skip_value(const char *s, size_t n, size_t *i) { return skip_value_d(s, n, i, 0); }

/* The whole of s is exactly one value opening with `open` ('{' or '['). */
static int is_one(const char *s, size_t n, char open) {
    size_t i = 0;
    skip_ws(s, n, &i);
    if (i >= n || s[i] != open) return 0;
    if (skip_value(s, n, &i) != 0) return 0;
    skip_ws(s, n, &i);
    return i == n;
}

/* The value of top-level `key` of the object s: 1 found (span in *v, *vl), 0 absent, -1 malformed. */
static int top_value(const char *s, size_t n, const char *key, const char **v, size_t *vl) {
    size_t i = 0, kl = strlen(key);
    skip_ws(s, n, &i);
    if (i >= n || s[i] != '{') return -1;
    i++;
    for (;;) {
        size_t ks, ke, vs;
        skip_ws(s, n, &i);
        if (i < n && s[i] == '}') return 0;
        ks = i;
        if (skip_string(s, n, &i) != 0) return -1;
        ke = i;
        skip_ws(s, n, &i);
        if (i >= n || s[i] != ':') return -1;
        i++;
        skip_ws(s, n, &i);
        vs = i;
        if (skip_value(s, n, &i) != 0) return -1;
        if (ke - ks == kl + 2 && memcmp(s + ks + 1, key, kl) == 0) {
            *v = s + vs;
            *vl = i - vs;
            return 1;
        }
        skip_ws(s, n, &i);
        if (i < n && s[i] == ',') {
            i++;
            continue;
        }
        return i < n && s[i] == '}' ? 0 : -1;
    }
}

/* ---- files ---- */

static int write_atomic(const char *path, const char *data, size_t len) {
    char tmp[600];
    int fd, ok;
    if (snprintf(tmp, sizeof tmp, "%s.tmp", path) >= (int)sizeof tmp) return -1;
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    ok = (size_t)write(fd, data, len) == len && fsync(fd) == 0;
    close(fd);
    if (!ok || rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

static char *read_all(const char *path, size_t max, size_t *len) {
    FILE *f = fopen(path, "rb");
    char *p;
    size_t n;
    if (!f) return NULL;
    p = malloc(max + 1);
    if (!p) {
        fclose(f);
        return NULL;
    }
    n = fread(p, 1, max + 1, f);
    fclose(f);
    if (n > max) { /* too large to be ours: ignore it */
        free(p);
        return NULL;
    }
    p[n] = 0;
    *len = n;
    return p;
}

static void path_of(char *out, size_t cap, const char *name) { snprintf(out, cap, "%s/v1/%s", M.dir, name); }

/* Under g_mu. */
static void save_devices(void) {
    size_t cap = (size_t)MESH_MAX_DEVICES * 512u;
    char *p, path[512], hex[65];
    buf_t b;
    int i;
    if (!M.persist) return;
    p = malloc(cap);
    if (!p) return;
    b.p = p;
    b.len = 0;
    b.cap = cap;
    b.err = 0;
    p[0] = 0;
    for (i = 0; i < MESH_MAX_DEVICES; i++) {
        dev_t_ *d = &M.dev[i];
        if (!d->used) continue;
        to_hex(d->key, hex);
        putf(&b, "%s\t%llu\t%llu\t%u\t%d\t%s\t%s\t%s\t%s\t%s\n", hex, (unsigned long long)d->first,
             (unsigned long long)d->last, (unsigned)d->ip, d->blocked, d->kind, d->os, d->app, d->name, d->nick);
    }
    path_of(path, sizeof path, "devices");
    if (!b.err) (void)write_atomic(path, b.p, b.len);
    /* How each was let in, beside it: an older helper reads v1/devices as it always did. */
    b.len = 0;
    b.err = 0;
    p[0] = 0;
    for (i = 0; i < MESH_MAX_DEVICES; i++) {
        dev_t_ *d = &M.dev[i];
        if (!d->used || !d->via[0]) continue;
        to_hex(d->key, hex);
        putf(&b, "%s\t%s\n", hex, d->via);
    }
    path_of(path, sizeof path, "via");
    if (!b.err) (void)write_atomic(path, b.p, b.len);
    free(p);
}

/* One of the words a device's `via` may hold, or "". */
static const char *via_word(const char *s) {
    static const char *const words[] = { "network", "tailscale", "approved" };
    size_t i;
    for (i = 0; i < sizeof words / sizeof words[0]; i++)
        if (!strcmp(s, words[i])) return words[i];
    return "";
}

static void save_settings(void) {
    char path[512], text[96];
    int n;
    if (!M.persist) return;
    n = snprintf(text, sizeof text, "require_approval=%d\nprune_days=%u\n", M.require_approval, M.prune_days);
    path_of(path, sizeof path, "settings");
    (void)write_atomic(path, text, (size_t)n);
}

static void load_settings(void) {
    char path[512], *p, *q;
    size_t n;
    M.require_approval = 0;
    M.prune_days = MESH_PRUNE_DEFAULT_DAYS;
    path_of(path, sizeof path, "settings");
    if (!(p = read_all(path, 4096, &n))) return;
    if ((q = strstr(p, "require_approval="))) M.require_approval = atoi(q + 17) != 0;
    if ((q = strstr(p, "prune_days="))) M.prune_days = (unsigned)strtoul(q + 11, NULL, 10);
    if (M.prune_days > 3650) M.prune_days = 3650;
    free(p);
}

/* Splits one line on tabs into at most `max` fields (in place). */
static int split_tabs(char *line, char **f, int max) {
    int n = 0;
    f[n++] = line;
    while (*line && n < max) {
        if (*line == '\t') {
            *line = 0;
            f[n++] = line + 1;
        }
        line++;
    }
    return n;
}

static void load_devices(void) {
    char path[512], *p, *line, *save = NULL;
    size_t n;
    int slot = 0;
    path_of(path, sizeof path, "devices");
    if (!(p = read_all(path, (size_t)MESH_MAX_DEVICES * 1024u, &n))) return;
    for (line = strtok_r(p, "\n", &save); line && slot < MESH_MAX_DEVICES; line = strtok_r(NULL, "\n", &save)) {
        char *f[10];
        dev_t_ *d = &M.dev[slot];
        if (split_tabs(line, f, 10) != 10) continue;
        memset(d, 0, sizeof *d);
        if (from_hex(f[0], d->key) != 0) continue;
        d->first = strtoull(f[1], NULL, 10);
        d->last = strtoull(f[2], NULL, 10);
        d->ip = (uint32_t)strtoul(f[3], NULL, 10);
        d->blocked = atoi(f[4]) != 0;
        clean_field(d->kind, sizeof d->kind, f[5]);
        clean_field(d->os, sizeof d->os, f[6]);
        clean_field(d->app, sizeof d->app, f[7]);
        clean_field(d->name, sizeof d->name, f[8]);
        clean_field(d->nick, sizeof d->nick, f[9]);
        d->used = 1;
        slot++;
    }
    free(p);
    path_of(path, sizeof path, "via");
    if (!(p = read_all(path, (size_t)MESH_MAX_DEVICES * 128u, &n))) return;
    save = NULL;
    for (line = strtok_r(p, "\n", &save); line; line = strtok_r(NULL, "\n", &save)) {
        char *f[2];
        uint8_t key[32];
        int i;
        if (split_tabs(line, f, 2) != 2 || from_hex(f[0], key) != 0) continue;
        for (i = 0; i < MESH_MAX_DEVICES; i++)
            if (M.dev[i].used && !memcmp(M.dev[i].key, key, 32))
                snprintf(M.dev[i].via, sizeof M.dev[i].via, "%s", via_word(f[1]));
    }
    free(p);
}

/* ---- state (under g_mu) ---- */

static dev_t_ *find_dev(const uint8_t key[32]) {
    int i;
    for (i = 0; i < MESH_MAX_DEVICES; i++)
        if (M.dev[i].used && memcmp(M.dev[i].key, key, 32) == 0) return &M.dev[i];
    return NULL;
}

/* A new record; when the list is full, the device unseen longest that is not blocked makes room. */
static dev_t_ *new_dev(const uint8_t key[32], uint8_t evicted[32], int *did_evict) {
    int i, pick = -1;
    *did_evict = 0;
    for (i = 0; i < MESH_MAX_DEVICES; i++)
        if (!M.dev[i].used) {
            pick = i;
            break;
        }
    if (pick < 0) {
        for (i = 0; i < MESH_MAX_DEVICES; i++)
            if (!M.dev[i].blocked && (pick < 0 || M.dev[i].last < M.dev[pick].last)) pick = i;
        if (pick < 0) return NULL;
        memcpy(evicted, M.dev[pick].key, 32);
        *did_evict = 1;
    }
    memset(&M.dev[pick], 0, sizeof M.dev[pick]);
    M.dev[pick].used = 1;
    memcpy(M.dev[pick].key, key, 32);
    return &M.dev[pick];
}

static void add_event(const char *kind, const uint8_t by[32], const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void add_event(const char *kind, const uint8_t by[32], const char *fmt, ...) {
    event_t_ *e = &M.ev[M.ev_seq % MESH_EVENTS_MAX];
    va_list ap;
    memset(e, 0, sizeof *e);
    e->seq = ++M.ev_seq;
    e->at = now_ms();
    snprintf(e->kind, sizeof e->kind, "%s", kind);
    if (by) memcpy(e->by, by, 32);
    va_start(ap, fmt);
    vsnprintf(e->text, sizeof e->text, fmt, ap);
    va_end(ap);
    clean_field(e->text, sizeof e->text, e->text);
    M.rev++;
}

static const char *shown_name(const dev_t_ *d) { return d->nick[0] ? d->nick : d->name[0] ? d->name : "A device"; }

static int is_online(const uint8_t key[32], const mesh_online_t *on, int non) {
    int i;
    for (i = 0; i < non; i++)
        if (memcmp(on[i].key, key, 32) == 0) return 1;
    return 0;
}

static uint64_t sig_of(const mesh_online_t *on, int non) {
    uint64_t s = 1469598103934665603ull; /* order-free: a sum of per-key hashes */
    int i, j;
    for (i = 0; i < non; i++) {
        uint64_t h = 1469598103934665603ull;
        for (j = 0; j < 32; j++) h = (h ^ on[i].key[j]) * 1099511628211ull;
        s += h;
    }
    return s + (uint64_t)non;
}

/* A lock is free when it ran out, or its holder is gone. */
static int lock_live(const lock_t_ *l, uint64_t t, const mesh_online_t *on, int non) {
    (void)t;
    return l->used && l->until_mono > mono_ms() && is_online(l->by, on, non);
}

/* Tailscale's addresses (100.64.0.0/10); `ip` in network order. */
static int is_tailnet_ip(uint32_t ip) {
    const uint8_t *b = (const uint8_t *)&ip;
    return b[0] == 100 && (b[1] & 0xC0u) == 64;
}

/* Under g_mu: asks past their time leave (a visible one changes the picture). */
static void expire_asks(void) {
    uint64_t m = mono_ms();
    int i;
    for (i = 0; i < MESH_ASKS_MAX; i++) {
        ask_t_ *a = &M.asks[i];
        if (!a->used || m - a->at_mono < ASK_TTL_MS) continue;
        if (a->state == ASK_ASKING) M.rev++;
        a->used = 0;
    }
}

static ask_t_ *find_ask(const uint8_t key[32]) {
    int i;
    for (i = 0; i < MESH_ASKS_MAX; i++)
        if (M.asks[i].used && !memcmp(M.asks[i].key, key, 32)) return &M.asks[i];
    return NULL;
}

/* A new ask. One address asks for one device at a time: a new key from the address of a device
 * still asking takes that ask's place (*same_ip set), so one stranger cannot fill the list by making
 * up keys. When all are taken, the oldest that is not allowed makes room: an Allow the owner just
 * gave holds until that device connects or its time is up. */
static ask_t_ *new_ask(const uint8_t key[32], uint32_t ip, int *same_ip) {
    int i, pick = -1;
    *same_ip = 0;
    if (ip)
        for (i = 0; i < MESH_ASKS_MAX; i++)
            if (M.asks[i].used && M.asks[i].state == ASK_ASKING && M.asks[i].ip == ip) {
                pick = i;
                *same_ip = 1;
                break;
            }
    if (pick < 0)
        for (i = 0; i < MESH_ASKS_MAX; i++)
            if (!M.asks[i].used) {
                pick = i;
                break;
            }
    if (pick < 0)
        for (i = 0; i < MESH_ASKS_MAX; i++)
            if (M.asks[i].state != ASK_ALLOWED && (pick < 0 || M.asks[i].at_mono < M.asks[pick].at_mono)) pick = i;
    if (pick < 0)
        for (i = 0; i < MESH_ASKS_MAX; i++)
            if (pick < 0 || M.asks[i].at_mono < M.asks[pick].at_mono) pick = i;
    memset(&M.asks[pick], 0, sizeof M.asks[pick]);
    M.asks[pick].used = 1;
    memcpy(M.asks[pick].key, key, 32);
    return &M.asks[pick];
}

/* ---- public ---- */

int mesh_start(const char *dir, const mesh_hooks_t *hooks, const char *helper_version) {
    char v1[512];
    int rc = 0;
    pthread_mutex_lock(&g_mu);
    memset(&M, 0, sizeof M);
    snprintf(M.dir, sizeof M.dir, "%s", dir ? dir : MESH_DIR);
    if (hooks) M.h = *hooks;
    snprintf(M.helper, sizeof M.helper, "%s", helper_version ? helper_version : "");
    snprintf(v1, sizeof v1, "%s/v1", M.dir);
    if ((mkdir(M.dir, 0700) != 0 && errno != EEXIST) || (mkdir(v1, 0700) != 0 && errno != EEXIST)) rc = -1;
    M.persist = rc == 0;
    load_settings();
    if (M.persist) load_devices();
    /* Revisions continue from the clock, so a client's "since" from before a restart never matches. */
    M.rev = now_ms();
    pthread_mutex_unlock(&g_mu);
    return rc;
}

int mesh_admit(const uint8_t key[32], const char *name, uint32_t ip) {
    uint8_t ev[32];
    int evicted = 0, verdict;
    const char *via = is_tailnet_ip(ip) ? "tailscale" : "network";
    dev_t_ *d;
    ask_t_ *a;
    pthread_mutex_lock(&g_mu);
    expire_asks();
    d = find_dev(key);
    a = find_ask(key);
    if (d && d->blocked) verdict = -1;
    else if (M.require_approval && a && a->state == ASK_ALLOWED) {
        verdict = 1;
        via = "approved";
        a->used = 0;
    } else if (M.require_approval) {
        verdict = 0;
        /* Asking: listed for the owner, once (a device that tries again is the same ask). A declined
         * one is not asked about again until its time is up. */
        if (!a) {
            int same_ip;
            a = new_ask(key, ip, &same_ip);
            a->state = ASK_ASKING;
            a->at = now_ms();
            a->at_mono = mono_ms();
            /* Told once per address: a stranger making up keys does not fill the activity list. */
            if (!same_ip) add_event("ask", key, "%s asks to connect", name && name[0] ? name : "A device");
            else M.rev++;
        }
        if (a->state == ASK_ASKING) {
            char was[NAME_MAX_];
            snprintf(was, sizeof was, "%s", a->name);
            clean_field(a->name, sizeof a->name, name);
            if (strcmp(was, a->name) != 0 || a->ip != ip) M.rev++;
            a->ip = ip;
        }
    } else
        verdict = 1;
    if (verdict == 1) {
        /* Listed now; "joined" is told once the device says who it is (hello), with its real name. */
        if (!d && (d = new_dev(key, ev, &evicted)) != NULL) {
            d->first = now_ms();
            clean_field(d->name, sizeof d->name, name);
            M.rev++;
        }
        if (d) {
            d->last = now_ms();
            d->ip = ip;
            snprintf(d->via, sizeof d->via, "%s", via);
        }
        save_devices();
    }
    pthread_mutex_unlock(&g_mu);
    if (evicted && M.h.forget) (void)M.h.forget(ev);
    return verdict;
}

int mesh_refused(const uint8_t key[32]) {
    dev_t_ *d;
    int r;
    pthread_mutex_lock(&g_mu);
    d = find_dev(key);
    r = d && d->blocked;
    pthread_mutex_unlock(&g_mu);
    return r;
}

static void put_settings(buf_t *b) {
    putf(b, "{\"require_approval\":%s,\"prune_days\":%u}", M.require_approval ? "true" : "false", M.prune_days);
}

static int do_hello(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap,
                    const mesh_online_t *on, int non) {
    char name[NAME_MAX_], kind[KIND_MAX], os[OS_MAX], app[APP_MAX], hex[65], tmp[NAME_MAX_];
    uint8_t ev[32];
    int evicted = 0, isnew = 0, i;
    uint32_t ip = 0;
    dev_t_ *d;
    buf_t b = { out, 0, cap, 0 };
    name[0] = kind[0] = os[0] = app[0] = 0;
    if (webui_json_str(body, n, "name", tmp, sizeof tmp) == 1) clean_field(name, sizeof name, tmp);
    if (webui_json_str(body, n, "kind", tmp, sizeof tmp) == 1) clean_field(kind, sizeof kind, tmp);
    if (webui_json_str(body, n, "os", tmp, sizeof tmp) == 1) clean_field(os, sizeof os, tmp);
    if (webui_json_str(body, n, "app", tmp, sizeof tmp) == 1) clean_field(app, sizeof app, tmp);
    for (i = 0; i < non; i++)
        if (memcmp(on[i].key, caller, 32) == 0) ip = on[i].ip;
    pthread_mutex_lock(&g_mu);
    d = find_dev(caller);
    if (!d) {
        if (!(d = new_dev(caller, ev, &evicted))) {
            pthread_mutex_unlock(&g_mu);
            return fail(out, cap, "devices_full");
        }
        d->first = now_ms();
    }
    /* Never said hello before (new, or listed at admission / as an older app): it joins now. */
    isnew = d->app[0] == 0;
    if (name[0]) snprintf(d->name, sizeof d->name, "%s", name);
    if (kind[0]) snprintf(d->kind, sizeof d->kind, "%s", kind);
    if (os[0]) snprintf(d->os, sizeof d->os, "%s", os);
    if (app[0]) snprintf(d->app, sizeof d->app, "%s", app);
    d->last = now_ms();
    if (ip) d->ip = ip;
    if (isnew) add_event("joined", caller, "%s connected for the first time", shown_name(d));
    /* A Docker engine whose /data was not kept comes back under a new key after every update: the
     * same name and address, kind docker, the old one offline. Only its list entry goes (its nickname
     * carries over); its key stays trusted, so if it was a different engine after all it simply comes
     * back. Phones and computers are never merged: default names and reused addresses are common. */
    for (i = 0; i < MESH_MAX_DEVICES && d->name[0] && d->ip && !strcmp(d->kind, "docker"); i++) {
        dev_t_ *o = &M.dev[i];
        if (!o->used || o == d || o->blocked || strcmp(o->name, d->name) != 0 || strcmp(o->kind, d->kind) != 0 ||
            o->ip != d->ip || is_online(o->key, on, non))
            continue;
        if (!d->nick[0] && o->nick[0]) snprintf(d->nick, sizeof d->nick, "%s", o->nick);
        o->used = 0;
        M.rev++;
    }
    save_devices();
    M.rev++;
    to_hex(caller, hex);
    puts_(&b, "{\"ok\":true,\"you\":");
    putstr(&b, hex);
    puts_(&b, ",\"helper\":");
    putstr(&b, M.helper);
    putf(&b, ",\"rev\":%llu,\"new\":%s,\"settings\":", (unsigned long long)M.rev, isnew ? "true" : "false");
    put_settings(&b);
    puts_(&b, "}");
    pthread_mutex_unlock(&g_mu);
    if (evicted && M.h.forget) (void)M.h.forget(ev);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

/* Under g_mu: devices unseen for prune_days (not blocked, not online) leave the list. Their keys are
 * copied out so the caller forgets them after unlocking. */
static int prune_locked(uint64_t t, const mesh_online_t *on, int non, uint8_t out[][32], int max) {
    int i, n = 0;
    uint64_t age = (uint64_t)M.prune_days * 86400000ull;
    if (!M.prune_days || t < age) return 0;
    for (i = 0; i < MESH_MAX_DEVICES && n < max; i++) {
        dev_t_ *d = &M.dev[i];
        if (!d->used || d->blocked || is_online(d->key, on, non) || d->last + age > t) continue;
        memcpy(out[n++], d->key, 32);
        d->used = 0;
    }
    if (n) {
        save_devices();
        M.rev++;
    }
    return n;
}

static int do_state(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap,
                    const mesh_online_t *on, int non) {
    uint64_t since = 0, ev_since = 0, t = now_ms(), sig;
    uint8_t (*pruned)[32] = calloc(2 * MESH_MAX_DEVICES, 32);
    int npruned = 0, i, first;
    char hex[65], ipt[16];
    buf_t b = { out, 0, cap, 0 };
    if (!pruned) return fail(out, cap, "out_of_memory");
    (void)webui_json_u64(body, n, "since", &since);
    (void)webui_json_u64(body, n, "ev", &ev_since);
    pthread_mutex_lock(&g_mu);
    {
        dev_t_ *me = find_dev(caller);
        if (me) me->last = t; /* not saved for a poll: the next hello or change writes it */
    }
    if (t - M.last_prune >= PRUNE_EVERY_MS) {
        M.last_prune = t;
        npruned = prune_locked(t, on, non, pruned, MESH_MAX_DEVICES);
    }
    /* A device on an app from before 8.0.0 never says hello: list it from its session. */
    for (i = 0; i < non; i++) {
        dev_t_ *d = find_dev(on[i].key);
        uint8_t ev[32];
        int evicted = 0;
        if (d) continue;
        if (!(d = new_dev(on[i].key, ev, &evicted))) break;
        if (evicted) memcpy(pruned[npruned++], ev, 32);
        d->first = d->last = t;
        d->ip = on[i].ip;
        clean_field(d->name, sizeof d->name, on[i].name);
        M.rev++;
        save_devices();
    }
    sig = sig_of(on, non);
    if (sig != M.online_sig) {
        M.online_sig = sig;
        M.rev++;
    }
    for (i = 0; i < MESH_LOCKS_MAX; i++)
        if (M.locks[i].used && !lock_live(&M.locks[i], t, on, non)) {
            M.locks[i].used = 0;
            M.rev++;
        }
    expire_asks();
    if (since == M.rev) {
        putf(&b, "{\"ok\":true,\"rev\":%llu,\"same\":true}", (unsigned long long)M.rev);
        goto done;
    }
    to_hex(caller, hex);
    putf(&b, "{\"ok\":true,\"rev\":%llu,\"now\":%llu,\"you\":\"%s\",\"helper\":", (unsigned long long)M.rev,
         (unsigned long long)t, hex);
    putstr(&b, M.helper);
    puts_(&b, ",\"settings\":");
    put_settings(&b);
    puts_(&b, ",\"asks\":[");
    first = 1;
    for (i = 0; i < MESH_ASKS_MAX; i++) {
        ask_t_ *a = &M.asks[i];
        if (!a->used || a->state != ASK_ASKING) continue;
        to_hex(a->key, hex);
        ip_text(a->ip, ipt);
        putf(&b, "%s{\"id\":\"%s\",\"name\":", first ? "" : ",", hex);
        putstr(&b, a->name);
        putf(&b, ",\"ip\":\"%s\",\"at\":%llu}", ipt, (unsigned long long)a->at);
        first = 0;
    }
    puts_(&b, "],\"devices\":[");
    first = 1;
    for (i = 0; i < MESH_MAX_DEVICES; i++) {
        dev_t_ *d = &M.dev[i];
        int online;
        if (!d->used) continue;
        online = is_online(d->key, on, non);
        to_hex(d->key, hex);
        ip_text(d->ip, ipt);
        putf(&b, "%s{\"id\":\"%s\",\"name\":", first ? "" : ",", hex);
        putstr(&b, d->name);
        puts_(&b, ",\"nick\":");
        putstr(&b, d->nick);
        puts_(&b, ",\"kind\":");
        putstr(&b, d->kind);
        puts_(&b, ",\"os\":");
        putstr(&b, d->os);
        puts_(&b, ",\"app\":");
        putstr(&b, d->app);
        puts_(&b, ",\"via\":");
        putstr(&b, d->via);
        putf(&b, ",\"first\":%llu,\"last\":%llu,\"ip\":\"%s\",\"online\":%s,\"blocked\":%s,\"activity\":",
             (unsigned long long)d->first, (unsigned long long)(online ? t : d->last), ipt, online ? "true" : "false",
             d->blocked ? "true" : "false");
        puts_(&b, online && d->activity[0] ? d->activity : "[]");
        puts_(&b, "}");
        first = 0;
    }
    puts_(&b, "],\"locks\":[");
    first = 1;
    for (i = 0; i < MESH_LOCKS_MAX; i++) {
        lock_t_ *l = &M.locks[i];
        if (!l->used) continue;
        to_hex(l->by, hex);
        putf(&b, "%s{\"name\":", first ? "" : ",");
        putstr(&b, l->name);
        puts_(&b, ",\"label\":");
        putstr(&b, l->label);
        putf(&b, ",\"by\":\"%s\",\"until\":%llu}", hex, (unsigned long long)lock_until_wall(l, t));
        first = 0;
    }
    puts_(&b, "],\"events\":[");
    first = 1;
    {
        uint64_t s = M.ev_seq > MESH_EVENTS_MAX ? M.ev_seq - MESH_EVENTS_MAX + 1 : 1;
        if (ev_since + 1 > s) s = ev_since + 1;
        for (; s <= M.ev_seq; s++) {
            event_t_ *e = &M.ev[(s - 1) % MESH_EVENTS_MAX];
            if (e->seq != s) continue;
            to_hex(e->by, hex);
            putf(&b, "%s{\"seq\":%llu,\"at\":%llu,\"kind\":", first ? "" : ",", (unsigned long long)e->seq,
                 (unsigned long long)e->at);
            putstr(&b, e->kind);
            putf(&b, ",\"by\":\"%s\",\"text\":", hex);
            putstr(&b, e->text);
            puts_(&b, "}");
            first = 0;
        }
    }
    putf(&b, "],\"ev\":%llu}", (unsigned long long)M.ev_seq);
done:
    pthread_mutex_unlock(&g_mu);
    for (i = 0; i < npruned; i++)
        if (M.h.forget) (void)M.h.forget(pruned[i]);
    free(pruned);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

static int do_activity(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap) {
    const char *v;
    size_t vl;
    dev_t_ *d;
    int r = webui_json_array(body, n, "items", &v, &vl);
    if (r != 1 || !is_one(v, vl, '[')) return fail(out, cap, "bad_request");
    if (vl > MESH_ACTIVITY_MAX) return fail(out, cap, "too_large");
    pthread_mutex_lock(&g_mu);
    d = find_dev(caller);
    if (!d) {
        pthread_mutex_unlock(&g_mu);
        return fail(out, cap, "say_hello_first");
    }
    if (strlen(d->activity) != vl || memcmp(d->activity, v, vl) != 0) {
        memcpy(d->activity, v, vl);
        d->activity[vl] = 0;
        M.rev++;
    }
    d->activity_at = now_ms();
    r = snprintf(out, cap, "{\"ok\":true,\"rev\":%llu}", (unsigned long long)M.rev);
    pthread_mutex_unlock(&g_mu);
    return r < 0 || (size_t)r >= cap ? -1 : r;
}

static int do_lock(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap,
                   const mesh_online_t *on, int non) {
    char op[16], name[LOCK_NAME_MAX], label[LABEL_MAX], hex[65], owner[OWNER_MAX];
    uint64_t ttl = LOCK_TTL_DEFAULT_S, t = now_ms();
    int force = 0, i, slot = -1, held = -1;
    buf_t b = { out, 0, cap, 0 };
    label[0] = 0;
    if (webui_json_str(body, n, "owner", owner, sizeof owner) != 1) owner[0] = 0;
    if (webui_json_str(body, n, "op", op, sizeof op) != 1 || webui_json_str(body, n, "name", name, sizeof name) != 1 ||
        !name[0])
        return fail(out, cap, "bad_request");
    (void)webui_json_u64(body, n, "ttl", &ttl);
    (void)webui_json_bool(body, n, "force", &force);
    if (webui_json_str(body, n, "label", label, sizeof label) == 1) clean_field(label, sizeof label, label);
    if (ttl == 0) ttl = LOCK_TTL_DEFAULT_S;
    if (ttl > LOCK_TTL_MAX_S) ttl = LOCK_TTL_MAX_S;
    pthread_mutex_lock(&g_mu);
    for (i = 0; i < MESH_LOCKS_MAX; i++) {
        lock_t_ *l = &M.locks[i];
        if (l->used && !lock_live(l, t, on, non)) {
            l->used = 0;
            M.rev++;
        }
        if (l->used && strcmp(l->name, name) == 0) held = i;
        else if (!l->used && slot < 0) slot = i;
    }
    if (!strcmp(op, "release")) {
        if (held >= 0 && (force || holds(&M.locks[held], caller, owner))) {
            M.locks[held].used = 0;
            M.rev++;
        }
        puts_(&b, "{\"ok\":true,\"got\":false}");
    } else if (!strcmp(op, "take")) {
        lock_t_ *l = NULL;
        if (held >= 0 && !holds(&M.locks[held], caller, owner) && !force) {
            lock_t_ *h = &M.locks[held];
            dev_t_ *hd = find_dev(h->by);
            int same_device = memcmp(h->by, caller, 32) == 0;
            to_hex(h->by, hex);
            putf(&b, "{\"ok\":true,\"got\":false,\"same_device\":%s,\"by\":\"%s\",\"by_name\":",
                 same_device ? "true" : "false", hex);
            putstr(&b, hd ? shown_name(hd) : "Another device");
            puts_(&b, ",\"label\":");
            putstr(&b, h->label);
            putf(&b, ",\"until\":%llu}", (unsigned long long)lock_until_wall(h, t));
        } else {
            if (held >= 0) {
                l = &M.locks[held];
                if (!holds(l, caller, owner)) {
                    dev_t_ *me = find_dev(caller), *was = find_dev(l->by);
                    add_event("takeover", caller, "%s took over from %s: %s", me ? shown_name(me) : "A device",
                              was ? shown_name(was) : "another device", l->label[0] ? l->label : l->name);
                }
            } else if (slot >= 0) {
                l = &M.locks[slot];
            }
            if (!l) {
                pthread_mutex_unlock(&g_mu);
                return fail(out, cap, "locks_full");
            }
            l->used = 1;
            snprintf(l->name, sizeof l->name, "%s", name);
            snprintf(l->label, sizeof l->label, "%s", label);
            memcpy(l->by, caller, 32);
            snprintf(l->owner, sizeof l->owner, "%s", owner);
            l->until_mono = mono_ms() + ttl * 1000u;
            M.rev++;
            putf(&b, "{\"ok\":true,\"got\":true,\"until\":%llu}", (unsigned long long)lock_until_wall(l, t));
        }
    } else {
        pthread_mutex_unlock(&g_mu);
        return fail(out, cap, "bad_op");
    }
    pthread_mutex_unlock(&g_mu);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

static int do_device(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap) {
    char op[16], id[80], name[NAME_MAX_ * 2];
    uint8_t key[32];
    int forget = 0, r;
    dev_t_ *d, *me;
    if (webui_json_str(body, n, "op", op, sizeof op) != 1 || webui_json_str(body, n, "id", id, sizeof id) != 1 ||
        from_hex(id, key) != 0)
        return fail(out, cap, "bad_request");
    pthread_mutex_lock(&g_mu);
    d = find_dev(key);
    me = find_dev(caller);
    if (!strcmp(op, "allow") || !strcmp(op, "decline")) {
        /* An answer to a device asking to connect (mesh_admit). */
        ask_t_ *a;
        expire_asks();
        a = find_ask(key);
        if (!a || a->state != ASK_ASKING) {
            pthread_mutex_unlock(&g_mu);
            return fail(out, cap, "not_found");
        }
        /* The console page names the address it was opened from ("from"): a device never allows
         * itself there (with LAN access on, any device on the network can open that page). */
        if (!strcmp(op, "allow")) {
            char from[16], at[16];
            if (webui_json_str(body, n, "from", from, sizeof from) == 1) {
                ip_text(a->ip, at);
                if (!strcmp(from, at)) {
                    pthread_mutex_unlock(&g_mu);
                    return fail(out, cap, "self_allow");
                }
            }
        }
        a->state = !strcmp(op, "allow") ? ASK_ALLOWED : ASK_DECLINED;
        a->at_mono = mono_ms(); /* allowed: its time to connect starts now */
        add_event(a->state == ASK_ALLOWED ? "allowed" : "declined", caller, "%s %s %s",
                  me ? shown_name(me) : "The console", a->state == ASK_ALLOWED ? "let in" : "turned away",
                  a->name[0] ? a->name : "a device");
        M.rev++;
        r = snprintf(out, cap, "{\"ok\":true,\"rev\":%llu}", (unsigned long long)M.rev);
        pthread_mutex_unlock(&g_mu);
        return r < 0 || (size_t)r >= cap ? -1 : r;
    }
    if (!d && strcmp(op, "block") != 0) {
        pthread_mutex_unlock(&g_mu);
        return fail(out, cap, "not_found");
    }
    if (!strcmp(op, "rename")) {
        if (webui_json_str(body, n, "name", name, sizeof name) != 1) {
            pthread_mutex_unlock(&g_mu);
            return fail(out, cap, "bad_request");
        }
        clean_field(d->nick, sizeof d->nick, name);
    } else if (!strcmp(op, "remove")) {
        add_event("removed", caller, "%s removed %s", me ? shown_name(me) : "A device", shown_name(d));
        d->used = 0;
        forget = 1;
    } else if (!strcmp(op, "block")) {
        if (!d) {
            uint8_t ev[32];
            int evicted = 0;
            if (!(d = new_dev(key, ev, &evicted))) {
                pthread_mutex_unlock(&g_mu);
                return fail(out, cap, "devices_full");
            }
            (void)evicted; /* the evicted one is merely forgotten from the list; it may reconnect */
        }
        d->blocked = 1;
        d->activity[0] = 0;
        add_event("blocked", caller, "%s blocked %s", me ? shown_name(me) : "A device", shown_name(d));
        forget = 1;
    } else if (!strcmp(op, "unblock")) {
        d->blocked = 0;
        add_event("unblocked", caller, "%s unblocked %s", me ? shown_name(me) : "A device", shown_name(d));
    } else {
        pthread_mutex_unlock(&g_mu);
        return fail(out, cap, "bad_op");
    }
    save_devices();
    M.rev++;
    r = snprintf(out, cap, "{\"ok\":true,\"rev\":%llu}", (unsigned long long)M.rev);
    pthread_mutex_unlock(&g_mu);
    if (forget && M.h.forget) (void)M.h.forget(key);
    return r < 0 || (size_t)r >= cap ? -1 : r;
}

static int do_settings(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap) {
    int ra, changed = 0;
    uint64_t days;
    buf_t b = { out, 0, cap, 0 };
    dev_t_ *me;
    pthread_mutex_lock(&g_mu);
    me = find_dev(caller);
    if (webui_json_bool(body, n, "require_approval", &ra) == 1 && ra != M.require_approval) {
        M.require_approval = ra;
        /* Off: whoever was asking gets in on its next try; there is nothing left to answer. */
        if (!ra) memset(M.asks, 0, sizeof M.asks);
        add_event("settings", caller, "%s turned %s approval for new devices", me ? shown_name(me) : "A device",
                  ra ? "on" : "off");
        changed = 1;
    }
    if (webui_json_u64(body, n, "prune_days", &days) == 1 && days != M.prune_days) {
        M.prune_days = days > 3650 ? 3650 : (unsigned)days;
        changed = 1;
    }
    if (changed) {
        save_settings();
        M.rev++;
    }
    puts_(&b, "{\"ok\":true,\"settings\":");
    put_settings(&b);
    puts_(&b, "}");
    pthread_mutex_unlock(&g_mu);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

static int doc_name_ok(const char *s) {
    size_t i, n = strlen(s);
    if (n == 0 || n > 40) return 0;
    for (i = 0; i < n; i++)
        if (!((s[i] >= 'a' && s[i] <= 'z') || (s[i] >= '0' && s[i] <= '9') || s[i] == '-' || s[i] == '_')) return 0;
    return 1;
}

/* A document file: "<rev>\n<json>". Under g_mu. *rev 0 and NULL when there is none. */
static char *doc_load(const char *name, uint64_t *rev, size_t *len) {
    char path[512], file[64], *p, *nl;
    size_t n;
    *rev = 0;
    *len = 0;
    snprintf(file, sizeof file, "doc-%s", name);
    path_of(path, sizeof path, file);
    if (!(p = read_all(path, MESH_DOC_MAX + 32, &n))) return NULL;
    nl = memchr(p, '\n', n);
    if (!nl || !is_one(nl + 1, n - (size_t)(nl + 1 - p), '{')) {
        free(p);
        return NULL;
    }
    *rev = strtoull(p, NULL, 10);
    *len = n - (size_t)(nl + 1 - p);
    memmove(p, nl + 1, *len);
    p[*len] = 0;
    return p;
}

static int doc_count(void) {
    char v1[512];
    DIR *dir;
    struct dirent *e;
    int n = 0;
    snprintf(v1, sizeof v1, "%s/v1", M.dir);
    if (!(dir = opendir(v1))) return 0;
    while ((e = readdir(dir)) != NULL)
        if (!strncmp(e->d_name, "doc-", 4) && !strstr(e->d_name, ".tmp")) n++;
    closedir(dir);
    return n;
}

static int do_doc_get(const char *body, size_t n, char *out, size_t cap) {
    char name[48];
    uint64_t rev;
    size_t len;
    char *doc;
    buf_t b = { out, 0, cap, 0 };
    if (webui_json_str(body, n, "name", name, sizeof name) != 1 || !doc_name_ok(name)) return fail(out, cap, "bad_request");
    pthread_mutex_lock(&g_mu);
    doc = doc_load(name, &rev, &len);
    pthread_mutex_unlock(&g_mu);
    putf(&b, "{\"ok\":true,\"rev\":%llu,\"doc\":", (unsigned long long)rev);
    if (doc) put(&b, doc, len);
    else puts_(&b, "null");
    puts_(&b, "}");
    free(doc);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

static int do_doc_put(const char *body, size_t n, char *out, size_t cap) {
    char name[48], path[512], file[64];
    uint64_t base = 0, rev;
    const char *v;
    size_t vl, len;
    char *cur, *text;
    int saved = 0;
    buf_t b = { out, 0, cap, 0 };
    if (webui_json_str(body, n, "name", name, sizeof name) != 1 || !doc_name_ok(name) ||
        webui_json_u64(body, n, "base", &base) != 1 || top_value(body, n, "doc", &v, &vl) != 1 || !is_one(v, vl, '{'))
        return fail(out, cap, "bad_request");
    if (vl > MESH_DOC_MAX) return fail(out, cap, "too_large");
    pthread_mutex_lock(&g_mu);
    cur = doc_load(name, &rev, &len);
    if (base == rev) {
        if (!cur && doc_count() >= MESH_DOCS_MAX) {
            pthread_mutex_unlock(&g_mu);
            return fail(out, cap, "docs_full");
        }
        text = malloc(vl + 32);
        if (!text) {
            pthread_mutex_unlock(&g_mu);
            free(cur);
            return fail(out, cap, "out_of_memory");
        }
        len = (size_t)snprintf(text, 32, "%llu\n", (unsigned long long)(rev + 1));
        memcpy(text + len, v, vl);
        snprintf(file, sizeof file, "doc-%s", name);
        path_of(path, sizeof path, file);
        if (M.persist && write_atomic(path, text, len + vl) == 0) {
            saved = 1;
            rev++;
            M.rev++;
        }
        free(text);
        if (!saved) {
            pthread_mutex_unlock(&g_mu);
            free(cur);
            return fail(out, cap, "io");
        }
        putf(&b, "{\"ok\":true,\"saved\":true,\"rev\":%llu}", (unsigned long long)rev);
    } else {
        /* Someone wrote since: here is what is there now, to merge into and try again. */
        putf(&b, "{\"ok\":true,\"saved\":false,\"rev\":%llu,\"doc\":", (unsigned long long)rev);
        if (cur) put(&b, cur, len);
        else puts_(&b, "null");
        puts_(&b, "}");
    }
    pthread_mutex_unlock(&g_mu);
    free(cur);
    return b.err ? fail(out, cap, "reply_too_large") : (int)b.len;
}

static int do_announce(const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap) {
    char kind[24], text[EVENT_TEXT_MAX];
    dev_t_ *me;
    int r;
    if (webui_json_str(body, n, "kind", kind, sizeof kind) != 1 || !kind[0]) return fail(out, cap, "bad_request");
    if (webui_json_str(body, n, "text", text, sizeof text) != 1) text[0] = 0;
    pthread_mutex_lock(&g_mu);
    me = find_dev(caller);
    add_event(kind, caller, "%s%s%s", me ? shown_name(me) : "A device", text[0] ? ": " : "", text);
    r = snprintf(out, cap, "{\"ok\":true,\"rev\":%llu}", (unsigned long long)M.rev);
    pthread_mutex_unlock(&g_mu);
    return r < 0 || (size_t)r >= cap ? -1 : r;
}

int mesh_call(int which, const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap) {
    mesh_online_t *on = NULL;
    int non = 0, r;
    if (!out || cap < 64) return -1;
    if (!body) {
        body = "{}";
        n = 2;
    }
    if (n == 0) {
        body = "{}";
        n = 2;
    }
    if (n > MESH_REQ_MAX) return fail(out, cap, "too_large");
    if (which == MESH_HELLO || which == MESH_STATE || which == MESH_LOCK) {
        /* Heap: the RPC workers' stacks are small. */
        if (!(on = calloc(MESH_MAX_DEVICES, sizeof *on))) return fail(out, cap, "out_of_memory");
        non = M.h.online ? M.h.online(on, MESH_MAX_DEVICES) : 0;
    }
    switch (which) {
    case MESH_HELLO: r = do_hello(caller, body, n, out, cap, on, non); break;
    case MESH_STATE: r = do_state(caller, body, n, out, cap, on, non); break;
    case MESH_ACTIVITY: r = do_activity(caller, body, n, out, cap); break;
    case MESH_LOCK: r = do_lock(caller, body, n, out, cap, on, non); break;
    case MESH_DEVICE: r = do_device(caller, body, n, out, cap); break;
    case MESH_SETTINGS: r = do_settings(caller, body, n, out, cap); break;
    case MESH_DOC_GET: r = do_doc_get(body, n, out, cap); break;
    case MESH_DOC_PUT: r = do_doc_put(body, n, out, cap); break;
    case MESH_ANNOUNCE: r = do_announce(caller, body, n, out, cap); break;
    default: r = fail(out, cap, "bad_method"); break;
    }
    free(on);
    return r;
}
