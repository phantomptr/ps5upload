/* cheats_list.c — the Cheats screen's title list. See include/cheats_list.h. */

#include "cheats_list.h"

#include <dirent.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <fcntl.h>
#include <unistd.h>

#define NAME_KEEP 128      /* cached name bytes, including the NUL */
#define FILE_NAME_MAX 256
#define MAX_FILES_PER_TITLE 16
#define MAX_SCAN_FILES 20000
#define CACHE_SLOTS 16384  /* power of two */
#define CACHE_PROBES 8
#define TAIL_RESERVE 160   /* room kept for the closing fields */

/* ── UTF-8 ───────────────────────────────────────────────────────── */

int cheats_utf8_char_len(const char *s) {
    const unsigned char *p = (const unsigned char *)s;
    unsigned char c = p[0];
    if (c == 0) return 0;
    if (c < 0x80) return 1;
    if (c >= 0xC2 && c <= 0xDF) return (p[1] & 0xC0) == 0x80 ? 2 : 0;
    if (c >= 0xE0 && c <= 0xEF) {
        if ((p[1] & 0xC0) != 0x80 || (p[2] & 0xC0) != 0x80) return 0;
        if (c == 0xE0 && p[1] < 0xA0) return 0; /* overlong */
        if (c == 0xED && p[1] >= 0xA0) return 0; /* surrogate */
        return 3;
    }
    if (c >= 0xF0 && c <= 0xF4) {
        if ((p[1] & 0xC0) != 0x80 || (p[2] & 0xC0) != 0x80 || (p[3] & 0xC0) != 0x80) return 0;
        if (c == 0xF0 && p[1] < 0x90) return 0; /* overlong */
        if (c == 0xF4 && p[1] >= 0x90) return 0; /* > U+10FFFF */
        return 4;
    }
    return 0;
}

size_t cheats_utf8_sanitize(const char *in, char *out, size_t cap) {
    size_t o = 0;
    if (cap == 0) return 0;
    while (*in) {
        int n = cheats_utf8_char_len(in);
        if (n == 0) {
            if (o + 1 >= cap) break;
            out[o++] = '?';
            in++;
            continue;
        }
        if (o + (size_t)n >= cap) break;
        memcpy(out + o, in, (size_t)n);
        o += (size_t)n;
        in += n;
    }
    out[o] = '\0';
    return o;
}

/* ── Per-file cache ──────────────────────────────────────────────── */

typedef struct {
    uint64_t key; /* FNV-1a of the path; 0 = empty slot */
    int64_t mtime;
    int64_t size;
    int mods; /* -1: the file did not parse */
    char name[NAME_KEEP];
} cache_slot_t;

static pthread_mutex_t g_cache_mu = PTHREAD_MUTEX_INITIALIZER;
static cache_slot_t *g_cache = NULL;
static unsigned long g_loads = 0;

unsigned long cheats_list_cache_loads(void) { return g_loads; }

void cheats_list_cache_reset(void) {
    pthread_mutex_lock(&g_cache_mu);
    free(g_cache);
    g_cache = NULL;
    g_loads = 0;
    pthread_mutex_unlock(&g_cache_mu);
}

static uint64_t fnv(const char *s) {
    uint64_t h = 1469598103934665603ULL;
    for (; *s; s++) h = (h ^ (unsigned char)*s) * 1099511628211ULL;
    return h ? h : 1;
}

/* The file's name and mod count, from the cache when its mtime and size are unchanged. */
static int file_summary(const cheats_list_cfg_t *cfg, const char *path, int fmt, char *name,
                        int *mods) {
    struct stat st;
    name[0] = '\0';
    *mods = -1;
    if (stat(path, &st) != 0) return -1;
    uint64_t key = fnv(path);
    cache_slot_t *victim = NULL;

    pthread_mutex_lock(&g_cache_mu);
    if (!g_cache) g_cache = (cache_slot_t *)calloc(CACHE_SLOTS, sizeof(cache_slot_t));
    if (g_cache) {
        for (int p = 0; p < CACHE_PROBES; p++) {
            cache_slot_t *s = &g_cache[(key + (uint64_t)p) & (CACHE_SLOTS - 1)];
            if (s->key == key) {
                if (s->mtime == (int64_t)st.st_mtime && s->size == (int64_t)st.st_size) {
                    snprintf(name, NAME_KEEP, "%s", s->name);
                    *mods = s->mods;
                    pthread_mutex_unlock(&g_cache_mu);
                    return 0;
                }
                victim = s; /* stale: replace in place */
                break;
            }
            if (s->key == 0 && !victim) victim = s;
        }
        if (!victim) victim = &g_cache[key & (CACHE_SLOTS - 1)];
    }
    pthread_mutex_unlock(&g_cache_mu);

    /* Miss: parse outside the lock (it allocates and reads the file). */
    char raw[256] = "";
    int n = 0;
    int rc = cfg->load(path, fmt, raw, sizeof raw, &n);
    pthread_mutex_lock(&g_cache_mu);
    g_loads++;
    if (rc == 0) {
        cheats_utf8_sanitize(raw, name, NAME_KEEP);
        *mods = n;
    }
    if (g_cache && victim) {
        victim->key = key;
        victim->mtime = (int64_t)st.st_mtime;
        victim->size = (int64_t)st.st_size;
        victim->mods = rc == 0 ? n : -1;
        snprintf(victim->name, NAME_KEEP, "%s", name);
    }
    pthread_mutex_unlock(&g_cache_mu);
    return rc == 0 ? 0 : -1;
}

/* ── Folder scan ─────────────────────────────────────────────────── */

typedef struct {
    char title[CHEATS_LIST_TITLE_MAX];
    char file[FILE_NAME_MAX];
    int dir; /* 0 json, 1 shn, 2 mc4 */
    unsigned seq;
} scan_ent_t;

static int scan_cmp(const void *a, const void *b) {
    const scan_ent_t *x = (const scan_ent_t *)a, *y = (const scan_ent_t *)b;
    int c = strcasecmp(x->title, y->title);
    if (c) return c;
    return x->seq < y->seq ? -1 : x->seq > y->seq;
}

/* The target-version segment of a cheat filename ("CUSA25234_01.08.shn" -> "01.08"). */
static void file_version(const char *filename, char *out, size_t cap) {
    if (cap) out[0] = '\0';
    const char *dot = strrchr(filename, '.');
    size_t stem_len = dot ? (size_t)(dot - filename) : strlen(filename);
    const char *us = memchr(filename, '_', stem_len);
    if (!us) return;
    const char *v = us + 1;
    const char *stem_end = filename + stem_len;
    const char *vend = v;
    while (vend < stem_end && *vend != '_') vend++;
    size_t vlen = (size_t)(vend - v);
    if (vlen == 0 || vlen >= cap) return;
    int has_dot = 0;
    for (size_t i = 0; i < vlen; i++) {
        char c = v[i];
        if (c == '.') has_dot = 1;
        else if (c < '0' || c > '9') return;
    }
    if (!has_dot) return;
    memcpy(out, v, vlen);
    out[vlen] = '\0';
}

/* Collects every cheat file of the three folders. *truncated is set past MAX_SCAN_FILES. */
static scan_ent_t *scan_all(const cheats_list_cfg_t *cfg, size_t *count, int *truncated) {
    static const char *const exts[3] = {".json", ".shn", ".mc4"};
    size_t n = 0, cap = 256;
    unsigned seq = 0;
    scan_ent_t *v = (scan_ent_t *)malloc(cap * sizeof *v);
    *count = 0;
    if (!v) return NULL;
    for (int d = 0; d < 3; d++) {
        DIR *dir = cfg->dirs[d] ? opendir(cfg->dirs[d]) : NULL;
        if (!dir) continue;
        struct dirent *de;
        while ((de = readdir(dir))) {
            const char *nm = de->d_name;
            if (nm[0] == '.') continue;
            const char *ext = strrchr(nm, '.');
            if (!ext || strcasecmp(ext, exts[d]) != 0) continue;
            size_t i = 0;
            while (nm[i] && nm[i] != '.' && nm[i] != '_' && i < CHEATS_LIST_TITLE_MAX - 1) i++;
            if (i < 4) continue;                     /* too short to be a title id */
            if (nm[i] != '.' && nm[i] != '_') continue; /* longer than a title id */
            if (strlen(nm) >= FILE_NAME_MAX) continue;
            if (n >= MAX_SCAN_FILES) {
                *truncated = 1;
                break;
            }
            if (n == cap) {
                scan_ent_t *g = (scan_ent_t *)realloc(v, cap * 2 * sizeof *v);
                if (!g) {
                    *truncated = 1;
                    break;
                }
                v = g;
                cap *= 2;
            }
            memcpy(v[n].title, nm, i);
            v[n].title[i] = '\0';
            snprintf(v[n].file, sizeof v[n].file, "%s", nm);
            v[n].dir = d;
            v[n].seq = seq++;
            n++;
        }
        closedir(dir);
    }
    qsort(v, n, sizeof *v, scan_cmp);
    *count = n;
    return v;
}

/* ── State sidecar ───────────────────────────────────────────────── */

/* Reads <state_dir>/<title>.json (at most 4096 bytes, as the toggle code does). Returns its length. */
static size_t read_state(const char *dir, const char *title, char *buf, size_t cap) {
    char path[400];
    if (!dir) return 0;
    snprintf(path, sizeof path, "%s/%s.json", dir, title);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size > 4096) {
        close(fd);
        return 0;
    }
    ssize_t rd = read(fd, buf, cap - 1);
    close(fd);
    if (rd <= 0) return 0;
    buf[rd] = '\0';
    return (size_t)rd;
}

/* How many of the first `mods` flat keys ("0","1",...) the sidecar marks true. */
static int count_enabled(const char *state, int mods) {
    int on = 0;
    for (int i = 0; i < mods; i++) {
        char key[16];
        snprintf(key, sizeof key, "\"%d\"", i);
        const char *p = strstr(state, key);
        if (!p) continue;
        p += strlen(key);
        while (*p && *p != ':') p++;
        if (*p != ':') continue;
        p++;
        while (*p == ' ' || *p == '\t' || *p == '\n') p++;
        if (strncmp(p, "true", 4) == 0) on++;
    }
    return on;
}

/* ── JSON ────────────────────────────────────────────────────────── */

typedef struct {
    char *b;
    size_t cap, off;
    int over;
} sb_t;

static void sb_raw(sb_t *s, const char *t) {
    size_t n = strlen(t);
    if (s->off + n >= s->cap) {
        s->over = 1;
        return;
    }
    memcpy(s->b + s->off, t, n + 1);
    s->off += n;
}

/* A JSON string body: escapes quote, backslash and newline, drops other control bytes, and
 * replaces anything that is not valid UTF-8 with '?'. */
static void sb_str(sb_t *s, const char *t) {
    while (*t) {
        unsigned char c = (unsigned char)*t;
        char tmp[8];
        size_t n = 0;
        if (c == '"' || c == '\\') {
            tmp[n++] = '\\';
            tmp[n++] = (char)c;
            t++;
        } else if (c == '\n') {
            tmp[n++] = '\\';
            tmp[n++] = 'n';
            t++;
        } else if (c < 0x20 || c == 0x7f) {
            t++;
            continue;
        } else if (c < 0x80) {
            tmp[n++] = (char)c;
            t++;
        } else {
            int k = cheats_utf8_char_len(t);
            if (k == 0) {
                tmp[n++] = '?';
                t++;
            } else {
                memcpy(tmp, t, (size_t)k);
                n = (size_t)k;
                t += k;
            }
        }
        if (s->off + n >= s->cap) {
            s->over = 1;
            return;
        }
        memcpy(s->b + s->off, tmp, n);
        s->off += n;
        s->b[s->off] = '\0';
    }
}

int cheats_list_build(const cheats_list_cfg_t *cfg, char *buf, size_t cap, size_t *written) {
    static const char *const fmt_names[4] = {"", "json", "shn", "mc4"};
    if (!cfg || !buf || cap < 256) return -1;
    size_t cnt = 0;
    int truncated = 0;
    scan_ent_t *v = scan_all(cfg, &cnt, &truncated);

    sb_t out = {buf, cap - TAIL_RESERVE, 0, 0};
    buf[0] = '\0';
    sb_raw(&out, "{\"titles\":[");
    int first = 1;

    for (size_t i = 0; v && i < cnt;) {
        size_t j = i;
        while (j < cnt && strcasecmp(v[j].title, v[i].title) == 0) j++;
        const char *title = v[i].title;

        int has_fmt[4] = {0, 0, 0, 0};
        char name[NAME_KEEP] = "";
        int mods[MAX_FILES_PER_TITLE];
        int nfiles = 0;
        for (size_t k = i; k < j && nfiles < MAX_FILES_PER_TITLE; k++, nfiles++) {
            char path[400], fname[NAME_KEEP];
            int m = -1;
            has_fmt[v[k].dir + 1] = 1;
            snprintf(path, sizeof path, "%s/%s", cfg->dirs[v[k].dir], v[k].file);
            if (file_summary(cfg, path, v[k].dir + 1, fname, &m) == 0 && !name[0] &&
                (cfg->usable_name ? cfg->usable_name(fname, title) : fname[0] != '\0'))
                snprintf(name, sizeof name, "%s", fname);
            mods[nfiles] = m;
        }
        int enabled = 0;
        char state[4096];
        int want_state = 0;
        for (int k = 0; k < nfiles; k++)
            if (mods[k] > 0) want_state = 1;
        if (want_state && read_state(cfg->state_dir, title, state, sizeof state))
            for (int k = 0; k < nfiles; k++)
                if (mods[k] > 0) enabled += count_enabled(state, mods[k]);

        char version[32];
        file_version(v[i].file, version, sizeof version);

        /* Build the entry on its own, then add it only if it fits whole. */
        char ent[2048];
        sb_t e = {ent, sizeof ent, 0, 0};
        ent[0] = '\0';
        if (!first) sb_raw(&e, ",");
        sb_raw(&e, "{\"title_id\":\"");
        sb_str(&e, title);
        sb_raw(&e, "\",\"name\":\"");
        sb_str(&e, name[0] ? name : title);
        sb_raw(&e, "\",\"version\":\"");
        sb_str(&e, version);
        sb_raw(&e, "\",\"formats\":[");
        int ff = 1;
        for (int f = 1; f <= 3; f++) {
            if (!has_fmt[f]) continue;
            if (!ff) sb_raw(&e, ",");
            sb_raw(&e, "\"");
            sb_raw(&e, fmt_names[f]);
            sb_raw(&e, "\"");
            ff = 0;
        }
        char tail[64];
        int is_running = cfg->running && cfg->running_title &&
                         strcasecmp(cfg->running_title, title) == 0;
        snprintf(tail, sizeof tail, "],\"enabled\":%d,\"running\":%s}", enabled,
                 is_running ? "true" : "false");
        sb_raw(&e, tail);
        if (!e.over) {
            if (out.off + e.off >= out.cap) {
                truncated = 1;
                break;
            }
            sb_raw(&out, ent);
            first = 0;
        }
        i = j;
    }
    free(v);

    /* The reserved tail: always room for these. */
    out.cap = cap;
    out.over = 0;
    sb_raw(&out, "],\"truncated\":");
    sb_raw(&out, truncated ? "true" : "false");
    sb_raw(&out, ",\"game_running\":");
    sb_raw(&out, cfg->running ? "true" : "false");
    sb_raw(&out, ",\"game_title_id\":\"");
    sb_str(&out, cfg->running_title ? cfg->running_title : "");
    sb_raw(&out, "\"}");
    if (written) *written = out.off;
    return 0;
}
