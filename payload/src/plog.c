/* The helper's log levels (plog.h). */
#include "plog.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

static const char *const CAT_NAMES[PLOG_NCATS] = {
    "general", "api",     "connection", "helper",   "rp",     "pairing", "wake", "transfer",
    "install", "cleanup", "files",      "convert",  "together", "golive", "mesh", "ui",
};

static const char *const LEVEL_NAMES[6] = {"?", "error", "warn", "info", "debug", "trace"};

volatile uint8_t plog_floor = PLOG_INFO;
volatile uint8_t plog_ceiling = PLOG_INFO;

/* The global level, the per-category levels (0: none, the global level counts), and when a raise
 * ends (monotonic seconds, 0: no raise). */
static volatile uint8_t s_level = PLOG_INFO;
static volatile uint8_t s_cats[PLOG_NCATS];
static volatile uint64_t s_until_s;

uint64_t plog_now_s(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (uint64_t)ts.tv_sec;
}

const char *plog_cat_name(int cat) {
    return cat >= 0 && cat < PLOG_NCATS ? CAT_NAMES[cat] : "general";
}

const char *plog_level_name(int lv) {
    return lv >= 1 && lv <= 5 ? LEVEL_NAMES[lv] : "?";
}

int plog_cat_by_name(const char *name, size_t len) {
    for (int i = 0; i < PLOG_NCATS; i++) {
        if (strlen(CAT_NAMES[i]) == len && memcmp(CAT_NAMES[i], name, len) == 0) return i;
    }
    return -1;
}

static int level_of(int cat) {
    uint8_t c = cat >= 0 && cat < PLOG_NCATS ? s_cats[cat] : 0;
    return c ? c : s_level;
}

static void recompute(void) {
    int lo = s_level, hi = s_level;
    for (int i = 0; i < PLOG_NCATS; i++) {
        int c = s_cats[i];
        if (!c) continue;
        if (c < lo) lo = c;
        if (c > hi) hi = c;
    }
    plog_floor = (uint8_t)(lo < PLOG_INFO ? lo : PLOG_INFO);
    plog_ceiling = (uint8_t)hi;
}

void plog_expire(uint64_t now_s) {
    if (plog_ceiling <= PLOG_INFO) return;
    if (s_until_s && now_s < s_until_s) return;
    if (s_level > PLOG_INFO) s_level = PLOG_INFO;
    for (int i = 0; i < PLOG_NCATS; i++) {
        if (s_cats[i] > PLOG_INFO) s_cats[i] = 0;
    }
    s_until_s = 0;
    recompute();
}

int plog_on(int lv, int cat) {
    if (lv <= plog_floor) return 1;
    if (lv > plog_ceiling) return 0;
    if (lv > PLOG_INFO) {
        plog_expire(plog_now_s());
        if (lv > plog_ceiling) return 0;
    }
    return lv <= level_of(cat);
}

void plog_write(int lv, int cat, const char *fmt, ...) {
    char line[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    fprintf(stderr, "[%s:%s] %s\n", plog_level_name(lv), plog_cat_name(cat), line);
}

void plog_reset(void) {
    s_level = PLOG_INFO;
    memset((void *)s_cats, 0, sizeof s_cats);
    s_until_s = 0;
    recompute();
}

void plog_without_pin(const char *msg, char *out, size_t cap) {
    if (!cap) return;
    const char *p = msg ? strstr(msg, "PIN") : NULL;
    if (!msg) {
        out[0] = '\0';
        return;
    }
    while (p) {
        const char *d = p + 3;
        while (*d == ':' || *d == ' ' || *d == '=') d++;
        const char *e = d;
        while (*e >= '0' && *e <= '9') e++;
        if (e - d >= 4 && d > p + 3) {
            snprintf(out, cap, "%.*s(shown on the TV)%s", (int)(d - msg), msg, e);
            return;
        }
        p = strstr(p + 3, "PIN");
    }
    snprintf(out, cap, "%s", msg);
}

/* ---- the setting as JSON ---- */

static size_t skip_ws(const char *s, size_t i, size_t n) {
    while (i < n && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n')) i++;
    return i;
}

/* An unsigned number at s[i]; -1 when there is none. */
static long long read_num(const char *s, size_t *i, size_t n) {
    size_t j = *i;
    long long v = 0;
    if (j >= n || s[j] < '0' || s[j] > '9') return -1;
    while (j < n && s[j] >= '0' && s[j] <= '9') {
        if (v < 100000000000LL) v = v * 10 + (s[j] - '0');
        j++;
    }
    *i = j;
    return v;
}

/* A "key" at s[i]: its text in k and klen, i after the closing quote. -1 when it is not one. */
static int read_key(const char *s, size_t *i, size_t n, const char **k, size_t *klen) {
    size_t j = *i;
    if (j >= n || s[j] != '"') return -1;
    j++;
    size_t start = j;
    while (j < n && s[j] != '"') {
        if (s[j] == '\\') return -1;
        j++;
    }
    if (j >= n) return -1;
    *k = s + start;
    *klen = j - start;
    *i = j + 1;
    return 0;
}

static int clamp_level(long long v) {
    return v < 1 ? 1 : v > 5 ? 5 : (int)v;
}

int plog_apply(const char *body, size_t len, uint64_t now_s) {
    size_t i = skip_ws(body ? body : "", 0, body ? len : 0);
    if (!body || i >= len) return 0;
    if (body[i] != '{') return -1;
    int level = -1;
    long long until = -1;
    int cats_given = 0;
    uint8_t cats[PLOG_NCATS];
    memset(cats, 0, sizeof cats);
    i++;
    for (;;) {
        i = skip_ws(body, i, len);
        if (i < len && body[i] == '}') break;
        const char *k;
        size_t kl;
        if (read_key(body, &i, len, &k, &kl) != 0) return -1;
        i = skip_ws(body, i, len);
        if (i >= len || body[i] != ':') return -1;
        i = skip_ws(body, i + 1, len);
        if (kl == 4 && memcmp(k, "cats", 4) == 0) {
            if (i >= len || body[i] != '{') return -1;
            cats_given = 1;
            i++;
            for (;;) {
                i = skip_ws(body, i, len);
                if (i < len && body[i] == '}') {
                    i++;
                    break;
                }
                const char *ck;
                size_t ckl;
                if (read_key(body, &i, len, &ck, &ckl) != 0) return -1;
                i = skip_ws(body, i, len);
                if (i >= len || body[i] != ':') return -1;
                i = skip_ws(body, i + 1, len);
                long long v = read_num(body, &i, len);
                if (v < 0) return -1;
                int c = plog_cat_by_name(ck, ckl);
                if (c >= 0) cats[c] = (uint8_t)clamp_level(v);
                i = skip_ws(body, i, len);
                if (i < len && body[i] == ',') i++;
            }
        } else {
            long long v = read_num(body, &i, len);
            if (v < 0) return -1;
            if (kl == 5 && memcmp(k, "level", 5) == 0) level = clamp_level(v);
            /* Never more than a day (the engine's longest raise), whatever the sender's clock did. */
            else if (kl == 7 && memcmp(k, "until_s", 7) == 0) until = v > 86400 ? 86400 : v;
        }
        i = skip_ws(body, i, len);
        if (i < len && body[i] == ',') {
            i++;
            continue;
        }
        if (i < len && body[i] == '}') break;
        return -1;
    }
    if (level > 0) s_level = (uint8_t)level;
    if (cats_given) {
        for (int c = 0; c < PLOG_NCATS; c++) s_cats[c] = cats[c];
    }
    if (until >= 0) s_until_s = until > 0 ? now_s + (uint64_t)until : 0;
    recompute();
    /* A raise with no time left is no raise. */
    if (plog_ceiling > PLOG_INFO && s_until_s == 0) {
        s_until_s = 1;
        plog_expire(now_s);
    }
    return 0;
}

int plog_describe(char *out, size_t cap, uint64_t now_s) {
    plog_expire(now_s);
    size_t at = 0;
    int n = snprintf(out, cap, "{\"ok\":true,\"level\":%d,\"cats\":{", (int)s_level);
    if (n < 0 || (size_t)n >= cap) return -1;
    at = (size_t)n;
    int first = 1;
    for (int c = 0; c < PLOG_NCATS; c++) {
        if (!s_cats[c]) continue;
        n = snprintf(out + at, cap - at, "%s\"%s\":%d", first ? "" : ",", CAT_NAMES[c], (int)s_cats[c]);
        if (n < 0 || (size_t)n >= cap - at) return -1;
        at += (size_t)n;
        first = 0;
    }
    uint64_t left = s_until_s && s_until_s > now_s ? s_until_s - now_s : 0;
    n = snprintf(out + at, cap - at, "},\"until_s\":%llu}", (unsigned long long)left);
    if (n < 0 || (size_t)n >= cap - at) return -1;
    return (int)(at + (size_t)n);
}
