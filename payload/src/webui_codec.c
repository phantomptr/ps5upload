/* Pure JSON <-> AVA1 adapters for the console web UI: see include/webui_codec.h. */
#include "webui_codec.h"

#include <stdio.h>
#include <string.h>

#include "ava1_app_gen.h"
#include "ava1_wire.h"
#include "config.h"

/* ---- a bounded JSON scanner (never reads past `e`, never recurses) ---- */

static const char *ws(const char *p, const char *e) {
    while (p < e && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) p++;
    return p;
}

/* p at '"': the byte after the closing quote, or NULL when the text ends first. */
static const char *skip_str(const char *p, const char *e) {
    for (p++; p < e; p++) {
        if (*p == '\\') {
            if (++p >= e) return NULL;
        } else if (*p == '"') {
            return p + 1;
        }
    }
    return NULL;
}

static int is_delim(char c) { return c == ',' || c == '}' || c == ']' || c == ' ' || c == '\t' || c == '\n' || c == '\r'; }

/* One value starting at p: the byte after it, or NULL. Containers are skipped by counting
 * brackets outside strings (iterative: a 64 KiB body of '[' cannot exhaust the stack). */
static const char *skip_val(const char *p, const char *e) {
    int depth = 0;
    if (p >= e) return NULL;
    if (*p == '"') return skip_str(p, e);
    if (*p != '{' && *p != '[') {
        const char *s = p;
        while (p < e && !is_delim(*p)) p++;
        return p > s ? p : NULL;
    }
    while (p < e) {
        if (*p == '"') {
            p = skip_str(p, e);
            if (!p) return NULL;
            continue;
        }
        if (*p == '{' || *p == '[') depth++;
        else if (*p == '}' || *p == ']') {
            if (--depth == 0) return p + 1;
        }
        p++;
    }
    return NULL;
}

/* The first byte of top-level `key`'s value in the object `json`, or NULL (absent, or not an
 * object). Keys are compared as they are written: a key spelled with escapes does not match. */
static const char *top_value(const char *json, size_t n, const char *key) {
    const char *e = json + n, *p = ws(json, e);
    size_t kl = strlen(key);
    if (!json || p >= e || *p != '{') return NULL;
    p++;
    for (;;) {
        const char *ks, *ke;
        p = ws(p, e);
        if (p >= e || *p != '"') return NULL;
        ks = p + 1;
        ke = skip_str(p, e);
        if (!ke) return NULL;
        p = ws(ke, e);
        if (p >= e || *p != ':') return NULL;
        p = ws(p + 1, e);
        if (p >= e) return NULL;
        if ((size_t)(ke - 1 - ks) == kl && memcmp(ks, key, kl) == 0) return p;
        p = skip_val(p, e);
        if (!p) return NULL;
        p = ws(p, e);
        if (p < e && *p == ',') {
            p++;
            continue;
        }
        return NULL;
    }
}

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Four hex digits at p (bounded by e): the value, or -1. */
static long hex4(const char *p, const char *e) {
    long v = 0;
    int i;
    if (e - p < 4) return -1;
    for (i = 0; i < 4; i++) {
        int h = hexval(p[i]);
        if (h < 0) return -1;
        v = v * 16 + h;
    }
    return v;
}

/* The JSON string at p ('"') unescaped into out (see webui_json_str): 1 with *after past its closing
 * quote, or -1. */
static int decode_str(const char *p, const char *e, char *out, size_t cap, const char **after) {
    size_t o = 0;
    if (cap) out[0] = '\0';
    if (p >= e || *p != '"' || cap == 0) return -1;
    for (p++; p < e; p++) {
        unsigned char c = (unsigned char)*p;
        unsigned long cp;
        uint8_t u[4];
        size_t k, ul;
        if (c == '"') {
            out[o] = '\0';
            *after = p + 1;
            return 1;
        }
        if (c < 0x20) return -1; /* a raw control character is not JSON */
        if (c != '\\') {
            if (o + 1 >= cap) return -1;
            out[o++] = (char)c;
            continue;
        }
        if (++p >= e) return -1;
        switch (*p) {
        case '"': cp = '"'; break;
        case '\\': cp = '\\'; break;
        case '/': cp = '/'; break;
        case 'b': cp = '\b'; break;
        case 'f': cp = '\f'; break;
        case 'n': cp = '\n'; break;
        case 'r': cp = '\r'; break;
        case 't': cp = '\t'; break;
        case 'u': {
            long v = hex4(p + 1, e);
            if (v < 0) return -1;
            p += 4;
            if (v >= 0xdc00 && v <= 0xdfff) return -1; /* a low surrogate first */
            if (v >= 0xd800 && v <= 0xdbff) {
                long lo;
                if (e - p < 7 || p[1] != '\\' || p[2] != 'u') return -1;
                lo = hex4(p + 3, e);
                if (lo < 0xdc00 || lo > 0xdfff) return -1;
                p += 6;
                v = 0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00);
            }
            if (v == 0) return -1; /* would cut a path short */
            cp = (unsigned long)v;
            break;
        }
        default:
            return -1;
        }
        if (cp < 0x80) {
            u[0] = (uint8_t)cp;
            ul = 1;
        } else if (cp < 0x800) {
            u[0] = (uint8_t)(0xc0 | (cp >> 6));
            u[1] = (uint8_t)(0x80 | (cp & 0x3f));
            ul = 2;
        } else if (cp < 0x10000) {
            u[0] = (uint8_t)(0xe0 | (cp >> 12));
            u[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3f));
            u[2] = (uint8_t)(0x80 | (cp & 0x3f));
            ul = 3;
        } else {
            u[0] = (uint8_t)(0xf0 | (cp >> 18));
            u[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3f));
            u[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3f));
            u[3] = (uint8_t)(0x80 | (cp & 0x3f));
            ul = 4;
        }
        if (o + ul >= cap) return -1;
        for (k = 0; k < ul; k++) out[o++] = (char)u[k];
    }
    out[0] = '\0';
    return -1; /* no closing quote */
}

int webui_json_str(const char *json, size_t n, const char *key, char *out, size_t cap) {
    const char *p = top_value(json, n, key), *after;
    if (cap) out[0] = '\0';
    if (!p) return 0;
    if (decode_str(p, json + n, out, cap, &after) != 1) {
        if (cap) out[0] = '\0';
        return -1;
    }
    return 1;
}

int webui_json_array_str(const char *arr, size_t len, size_t *pos, char *out, size_t cap) {
    const char *e, *p, *after;
    if (cap) out[0] = '\0';
    if (!arr || *pos > len) return -1;
    e = arr + len;
    if (*pos == len && len) return 0; /* ended before */
    if (*pos == 0) {
        p = ws(arr, e);
        if (p >= e || *p != '[') return -1;
        p = ws(p + 1, e);
        if (p < e && *p == ']') {
            *pos = len;
            return 0;
        }
    } else {
        p = ws(arr + *pos, e);
        if (p < e && *p == ']') {
            *pos = len;
            return 0;
        }
        if (p >= e || *p != ',') return -1;
        p = ws(p + 1, e);
    }
    if (decode_str(p, e, out, cap, &after) != 1) {
        if (cap) out[0] = '\0';
        return -1;
    }
    *pos = (size_t)(after - arr);
    return 1;
}

int webui_json_u64(const char *json, size_t n, const char *key, uint64_t *out) {
    const char *e = json + n, *p = top_value(json, n, key);
    uint64_t v = 0;
    int digits = 0;
    if (!p) return 0;
    while (p < e && *p >= '0' && *p <= '9') {
        uint64_t d = (uint64_t)(*p - '0');
        if (v > (UINT64_MAX - d) / 10) return -1;
        v = v * 10 + d;
        p++;
        digits++;
    }
    if (!digits || (p < e && !is_delim(*p))) return -1;
    *out = v;
    return 1;
}

int webui_json_bool(const char *json, size_t n, const char *key, int *out) {
    const char *e = json + n, *p = top_value(json, n, key);
    if (!p) return 0;
    if (e - p >= 4 && memcmp(p, "true", 4) == 0 && (e - p == 4 || is_delim(p[4]))) {
        *out = 1;
        return 1;
    }
    if (e - p >= 5 && memcmp(p, "false", 5) == 0 && (e - p == 5 || is_delim(p[5]))) {
        *out = 0;
        return 1;
    }
    return -1;
}

int webui_json_null(const char *json, size_t n, const char *key) {
    const char *e = json + n, *p = top_value(json, n, key);
    return p && e - p >= 4 && memcmp(p, "null", 4) == 0 && (e - p == 4 || is_delim(p[4]));
}

int webui_json_array(const char *json, size_t n, const char *key, const char **val, size_t *len) {
    const char *e = json + n, *p = top_value(json, n, key), *end;
    if (!p) return 0;
    if (*p != '[') return -1;
    end = skip_val(p, e);
    if (!end) return -1;
    *val = p;
    *len = (size_t)(end - p);
    return 1;
}

/* A signed integer value (focus ids are -1 when unknown): 1, 0 absent, -1 not an integer. */
static int json_i64(const char *json, size_t n, const char *key, long long *out) {
    const char *e = json + n, *p = top_value(json, n, key);
    long long v = 0;
    int neg = 0, digits = 0;
    if (!p) return 0;
    if (*p == '-') {
        neg = 1;
        p++;
    }
    while (p < e && *p >= '0' && *p <= '9' && digits < 18) {
        v = v * 10 + (*p - '0');
        p++;
        digits++;
    }
    if (!digits || (p < e && !is_delim(*p))) return -1;
    *out = neg ? -v : v;
    return 1;
}

/* ---- escaping ---- */

typedef struct {
    char *out;
    size_t cap, len; /* len: the full length, written or not */
    size_t wrote;    /* the bytes actually written */
    int full;        /* a piece did not fit: nothing after it is written (no split escape, no gap) */
} jw_t;

static void put(jw_t *w, const char *s, size_t k) {
    if (!w->full && w->cap && w->len + k <= w->cap - 1) {
        memcpy(w->out + w->len, s, k);
        w->wrote = w->len + k;
    } else {
        w->full = 1;
    }
    w->len += k;
}

/* The length of the valid UTF-8 sequence at s (n bytes left), or 0. */
static size_t utf8_seq(const unsigned char *s, size_t n) {
    unsigned c = s[0];
    unsigned long cp;
    size_t k, i;
    if (c >= 0xc2 && c <= 0xdf) k = 2, cp = c & 0x1f;
    else if (c >= 0xe0 && c <= 0xef) k = 3, cp = c & 0x0f;
    else if (c >= 0xf0 && c <= 0xf4) k = 4, cp = c & 0x07;
    else return 0;
    if (n < k) return 0;
    for (i = 1; i < k; i++) {
        if ((s[i] & 0xc0) != 0x80) return 0;
        cp = (cp << 6) | (s[i] & 0x3f);
    }
    if ((k == 3 && cp < 0x800) || (k == 4 && (cp < 0x10000 || cp > 0x10ffff)) || (cp >= 0xd800 && cp <= 0xdfff))
        return 0;
    return k;
}

static void esc(jw_t *w, const char *s, size_t n) {
    static const char hx[] = "0123456789abcdef";
    size_t i = 0;
    while (i < n) {
        unsigned char c = (unsigned char)s[i];
        if (c == '"') put(w, "\\\"", 2);
        else if (c == '\\') put(w, "\\\\", 2);
        else if (c == '\n') put(w, "\\n", 2);
        else if (c == '\r') put(w, "\\r", 2);
        else if (c == '\t') put(w, "\\t", 2);
        else if (c == '\b') put(w, "\\b", 2);
        else if (c == '\f') put(w, "\\f", 2);
        else if (c < 0x20) {
            char u[6] = { '\\', 'u', '0', '0', hx[c >> 4], hx[c & 15] };
            put(w, u, 6);
        } else if (c < 0x80) {
            put(w, (const char *)&s[i], 1);
        } else {
            size_t k = utf8_seq((const unsigned char *)s + i, n - i);
            if (k) {
                put(w, s + i, k);
                i += k;
                continue;
            }
            put(w, "\\ufffd", 6);
        }
        i++;
    }
}

size_t webui_json_escape(char *out, size_t cap, const char *s, size_t n) {
    jw_t w;
    memset(&w, 0, sizeof w);
    w.out = out;
    w.cap = out ? cap : 0;
    esc(&w, s, n);
    if (w.cap) out[w.wrote] = '\0';
    return w.len;
}

/* ---- fs.list ---- */

static const char *entry_kind(uint8_t k) {
    switch (k) {
    case AVA1_ENTRY_FILE: return "file";
    case AVA1_ENTRY_DIR: return "dir";
    case AVA1_ENTRY_LINK: return "link";
    default: return "other";
    }
}

/* Room kept for the list's tail: `],"more":false,"next":<20 digits>}` and the NUL. */
#define LIST_TAIL_ROOM 48u

int webui_list_to_json(const uint8_t *res, size_t n, char *out, size_t cap) {
    ava1_fs_list_result_t r;
    ava1_fs_entry_t ent;
    ava1_r_t it;
    size_t pos;
    uint32_t written = 0, skipped;
    uint64_t next;
    int more, rc;
    if (cap) out[0] = '\0';
    if (ava1_fs_list_result_decode(res, n, &r) != 0) return -1;
    if (cap < sizeof "{\"entries\":[" - 1 + LIST_TAIL_ROOM) return -1;
    pos = (size_t)snprintf(out, cap, "{\"entries\":[");
    more = r.more != 0;
    ava1_r_init(&it, r.entries, r.entries_len);
    while ((rc = ava1_fs_entry_next(&it, &ent)) == 1) {
        char nums[80];
        size_t name_len = webui_json_escape(NULL, 0, (const char *)ent.name, ent.name_len);
        const char *kind = entry_kind(ent.kind);
        int nl;
        size_t need;
        if (ent.has_mtime)
            nl = snprintf(nums, sizeof nums, ",\"size\":%llu,\"mtime\":%llu}", (unsigned long long)ent.size,
                          (unsigned long long)ent.mtime);
        else
            nl = snprintf(nums, sizeof nums, ",\"size\":%llu,\"mtime\":null}", (unsigned long long)ent.size);
        need = (written ? 1u : 0u) + sizeof "{\"name\":\"" - 1 + name_len + sizeof "\",\"kind\":\"\"" - 1 + strlen(kind) +
               (size_t)nl;
        if (pos + need + LIST_TAIL_ROOM > cap) {
            more = 1;
            break;
        }
        if (written) out[pos++] = ',';
        memcpy(out + pos, "{\"name\":\"", 9);
        pos += 9;
        webui_json_escape(out + pos, name_len + 1, (const char *)ent.name, ent.name_len);
        pos += name_len;
        pos += (size_t)snprintf(out + pos, cap - pos, "\",\"kind\":\"%s\"%s", kind, nums);
        written++;
    }
    if (rc < 0) {
        out[0] = '\0';
        return -1;
    }
    skipped = r.entries_count > written ? r.entries_count - written : 0;
    next = r.total_scanned >= skipped ? (uint64_t)(r.total_scanned - skipped) : 0;
    snprintf(out + pos, cap - pos, "],\"more\":%s,\"next\":%llu}", more ? "true" : "false", (unsigned long long)next);
    return (int)written;
}

/* ---- job list ---- */

void webui_hex16(const uint8_t id[16], char out[33]) {
    static const char hx[] = "0123456789abcdef";
    int i;
    for (i = 0; i < 16; i++) {
        out[2 * i] = hx[id[i] >> 4];
        out[2 * i + 1] = hx[id[i] & 15];
    }
    out[32] = '\0';
}

int webui_parse_hex16(const char *s, uint8_t id[16]) {
    int i;
    if (!s || strlen(s) != 32) return -1;
    for (i = 0; i < 16; i++) {
        int a = hexval(s[2 * i]), b = hexval(s[2 * i + 1]);
        if (a < 0 || b < 0) return -1;
        id[i] = (uint8_t)(a * 16 + b);
    }
    return 0;
}

static const char *job_kind(uint8_t k) {
    switch (k) {
    case AVA1_JOB_UPLOAD: return "upload";
    case AVA1_JOB_DOWNLOAD: return "download";
    case AVA1_JOB_COPY: return "copy";
    case 4: return "op"; /* AVA1_JOB_OPKIND (ava1_op.h) */
    default: return "other";
    }
}

int webui_jobs_to_json(const uint8_t *res, size_t n, int (*mine)(const uint8_t id[16], void *ctx), void *ctx,
                       char *out, size_t cap) {
    ava1_job_list_result_t r;
    ava1_job_entry_t e;
    ava1_r_t it;
    size_t pos;
    int written = 0, rc;
    if (cap) out[0] = '\0';
    if (ava1_job_list_result_decode(res, n, &r) != 0) return -1;
    if (cap < 16) return -1;
    pos = (size_t)snprintf(out, cap, "{\"jobs\":[");
    ava1_r_init(&it, r.jobs, r.jobs_len);
    while ((rc = ava1_job_entry_next(&it, &e)) == 1) {
        char hex[33];
        int k;
        webui_hex16(e.job_id, hex);
        k = snprintf(out + pos, cap - pos,
                     "%s{\"job\":\"%s\",\"kind\":\"%s\",\"state\":\"%s\",\"files_done\":%lu,\"files_total\":%lu,"
                     "\"bytes_done\":%llu,\"bytes_total\":%llu,\"mine\":%s}",
                     written ? "," : "", hex, job_kind(e.kind),
                     e.state == 0 ? "running" : (e.state == 1 ? "done" : "failed"), (unsigned long)e.files_done,
                     (unsigned long)e.files_total, (unsigned long long)e.bytes_done, (unsigned long long)e.bytes_total,
                     mine && mine(e.job_id, ctx) ? "true" : "false");
        if (k < 0 || (size_t)k >= cap - pos) {
            out[0] = '\0';
            return -1;
        }
        pos += (size_t)k;
        written++;
    }
    if (rc < 0 || cap - pos < 3) {
        out[0] = '\0';
        return -1;
    }
    memcpy(out + pos, "]}", 3);
    return written;
}

/* Appends `"key":"<escaped>"` or `"key":null`; 0, or -1 when it does not fit. */
static int put_jstr(char *out, size_t cap, size_t *pos, const char *key, const char *s, size_t n) {
    size_t k, need;
    if (!s) {
        k = (size_t)snprintf(out + *pos, cap - *pos, "\"%s\":null", key);
        if (k >= cap - *pos) return -1;
        *pos += k;
        return 0;
    }
    k = (size_t)snprintf(out + *pos, cap - *pos, "\"%s\":\"", key);
    if (k >= cap - *pos) return -1;
    *pos += k;
    need = webui_json_escape(out + *pos, cap - *pos, s, n);
    if (need >= cap - *pos || cap - *pos - need < 2) return -1;
    *pos += need;
    out[(*pos)++] = '"';
    out[*pos] = '\0';
    return 0;
}

int webui_job_status_to_json(const uint8_t *res, size_t n, char *out, size_t cap) {
    ava1_status_t s;
    char hex[33];
    size_t pos;
    int k;
    if (cap) out[0] = '\0';
    if (cap < 16 || ava1_status_decode(res, n, &s) != 0) return -1;
    webui_hex16(s.job_id, hex);
    k = snprintf(out, cap,
                 "{\"job\":\"%s\",\"state\":\"%s\",\"files_done\":%lu,\"files_total\":%lu,\"bytes_done\":%llu,"
                 "\"bytes_total\":%llu,",
                 hex, !s.has_state || s.state == 0 ? "running" : (s.state == 1 ? "done" : "failed"),
                 (unsigned long)s.files_done, (unsigned long)s.files_total, (unsigned long long)s.bytes_durable,
                 (unsigned long long)s.bytes_total);
    if (k < 0 || (size_t)k >= cap) goto fail;
    pos = (size_t)k;
    if (put_jstr(out, cap, &pos, "message", s.has_current && s.current_len ? (const char *)s.current : NULL,
                 s.current_len) != 0)
        goto fail;
    if (cap - pos < 2) goto fail;
    out[pos++] = ',';
    /* the result is the operation's own JSON text: carried as a string the page parses */
    if (put_jstr(out, cap, &pos, "result",
                 s.has_result && s.result_len && s.result_len <= WEBUI_JOB_RESULT_MAX ? (const char *)s.result : NULL,
                 s.result_len) != 0)
        goto fail;
    if (s.has_code) k = snprintf(out + pos, cap - pos, ",\"code\":%u}", (unsigned)s.code);
    else k = snprintf(out + pos, cap - pos, ",\"code\":null}");
    if (k < 0 || (size_t)k >= cap - pos) goto fail;
    return 0;
fail:
    out[0] = '\0';
    return -1;
}

static int name_ok_max(const char *s, size_t max) {
    size_t i, n = s ? strlen(s) : 0;
    if (n == 0 || n > max || strcmp(s, ".") == 0 || strcmp(s, "..") == 0) return 0;
    for (i = 0; i < n; i++)
        if (s[i] == '/' || (unsigned char)s[i] < 0x20) return 0;
    return 1;
}

int webui_name_ok(const char *s) { return name_ok_max(s, 255); }

int webui_unpack_name_ok(const char *s) { return name_ok_max(s, WEBUI_UNPACK_NAME_MAX); }

int webui_rename_detour(const char *from, const char *to, unsigned salt, char *tmp, size_t cap) {
    const char *fs, *ts;
    size_t dl;
    int k;
    if (!tmp || !cap) return -1;
    tmp[0] = '\0';
    if (!from || !to || strcmp(from, to) == 0) return -1;
    fs = strrchr(from, '/');
    ts = strrchr(to, '/');
    if (!fs || !ts || !fs[1] || !ts[1]) return -1;
    dl = (size_t)(fs - from);
    if ((size_t)(ts - to) != dl || memcmp(from, to, dl) != 0) return -1;
    if (strcmp(fs + 1, ".") == 0 || strcmp(fs + 1, "..") == 0 || strcmp(ts + 1, ".") == 0 || strcmp(ts + 1, "..") == 0)
        return -1;
    k = snprintf(tmp, cap, "%.*s/.ps5upload-rename-%08x", (int)dl, from, salt);
    if (k < 0 || (size_t)k >= cap) {
        tmp[0] = '\0';
        return -1;
    }
    return 0;
}

int webui_path_job_args(char *out, size_t cap, const char *path, const char *name, const char *tail) {
    size_t pos = 0, k;
    if (!cap) return -1;
    out[0] = '\0';
    if (!path || !path[0]) return -1;
    if (cap < 2) return -1;
    out[pos++] = '{';
    if (put_jstr(out, cap, &pos, "path", path, strlen(path)) != 0) goto fail;
    if (name) {
        if (cap - pos < 2) goto fail;
        out[pos++] = ',';
        if (put_jstr(out, cap, &pos, "name", name, strlen(name)) != 0) goto fail;
    }
    k = (size_t)snprintf(out + pos, cap - pos, "%s}", tail ? tail : "");
    if (k >= cap - pos) goto fail;
    return (int)(pos + k);
fail:
    out[0] = '\0';
    return -1;
}

/* ---- errors, types, small parsers ---- */

/* The number after "errno_" in cause, or -1. */
static long cause_errno(const char *cause) {
    const char *p = cause ? strstr(cause, "errno_") : NULL;
    long v = 0;
    int d = 0;
    if (!p) return -1;
    for (p += 6; *p >= '0' && *p <= '9' && d < 9; p++, d++) v = v * 10 + (*p - '0');
    return d ? v : -1;
}

int webui_http_status_for(int st, const char *cause) {
    switch (st) {
    case AVA1_ERR_PROTOCOL: return 400;
    case AVA1_ERR_PATH: return 403;
    case AVA1_ERR_UNKNOWN_JOB: return 404;
    case AVA1_ERR_EXISTS:
    case AVA1_ERR_BUSY:
    case AVA1_ERR_CROSS_DEVICE: return 409;
    case AVA1_ERR_IO: {
        long e = cause_errno(cause);
        if (e == 2) return 404; /* ENOENT */
        if (e < 0 && cause && strstr(cause, "fs_read_stat_failed")) return 404; /* fs.read's ENOENT/ENOTDIR */
        return 500;
    }
    default: return 500;
    }
}

static int ext_is(const char *ext, const char *want) {
    size_t i;
    for (i = 0; want[i]; i++) {
        char c = ext[i];
        if (c >= 'A' && c <= 'Z') c = (char)(c - 'A' + 'a');
        if (c != want[i]) return 0;
    }
    return ext[i] == '\0';
}

const char *webui_mime_for(const char *path) {
    const char *slash = strrchr(path, '/'), *dot = strrchr(path, '.');
    if (!dot || (slash && dot < slash) || dot == path || (slash && dot == slash + 1)) return "application/octet-stream";
    dot++;
    if (ext_is(dot, "png")) return "image/png";
    if (ext_is(dot, "jpg") || ext_is(dot, "jpeg")) return "image/jpeg";
    if (ext_is(dot, "txt") || ext_is(dot, "log") || ext_is(dot, "ini")) return "text/plain; charset=utf-8";
    if (ext_is(dot, "json")) return "application/json";
    return "application/octet-stream";
}

/* The value of `key=` at a line start: its first byte and length (to the line end), or NULL. */
static const char *kv_find(const char *t, size_t n, const char *key, size_t *len) {
    size_t kl = strlen(key), i = 0;
    while (i < n) {
        size_t s = i, e;
        while (i < n && t[i] != '\n') i++;
        e = i;
        if (e > s && t[e - 1] == '\r') e--;
        if (e - s > kl && memcmp(t + s, key, kl) == 0 && t[s + kl] == '=') {
            *len = e - s - kl - 1;
            return t + s + kl + 1;
        }
        i++;
    }
    return NULL;
}

int webui_kv_str(const char *text, size_t n, const char *key, char *out, size_t cap) {
    size_t len = 0;
    const char *v = kv_find(text, n, key, &len);
    if (cap) out[0] = '\0';
    if (!v) return 0;
    if (cap) {
        if (len > cap - 1) len = cap - 1;
        memcpy(out, v, len);
        out[len] = '\0';
    }
    return 1;
}

int webui_kv_int(const char *text, size_t n, const char *key, long *out) {
    size_t len = 0, i = 0;
    const char *v = kv_find(text, n, key, &len);
    long x = 0;
    int neg = 0;
    if (!v) return 0;
    if (len && v[0] == '-') {
        neg = 1;
        i = 1;
    }
    if (i >= len || len - i > 12) return -1;
    for (; i < len; i++) {
        if (v[i] < '0' || v[i] > '9') return -1;
        x = x * 10 + (v[i] - '0');
    }
    *out = neg ? -x : x;
    return 1;
}

/* `"key":N` (or null when the value is absent, not an integer, or at most `none`) after a comma. */
static size_t fan_put_int(char *out, size_t cap, size_t at, const char *text, size_t n, const char *key, long none) {
    long v = 0;
    int k;
    if (at >= cap) return at;
    if (webui_kv_int(text, n, key, &v) == 1 && v > none) k = snprintf(out + at, cap - at, ",\"%s\":%ld", key + 4, v);
    else k = snprintf(out + at, cap - at, ",\"%s\":null", key + 4);
    return k < 0 ? cap : at + (size_t)k;
}

int webui_fan_status_json(const char *text, size_t n, char *out, size_t cap) {
    char guard[16], filtered[16];
    long cpu = 0, soc = 0, pinned = 0, min_c = 45, max_c = 85, restores = 0;
    size_t at;
    int k;
    if (!cap) return -1;
    if (webui_kv_int(text, n, "cpu_temp", &cpu) != 1 || cpu <= 0) cpu = 0;
    if (webui_kv_int(text, n, "soc_temp", &soc) != 1 || soc <= 0) soc = 0;
    if (webui_kv_int(text, n, "fan_pinned_c", &pinned) != 1 || pinned < 0) pinned = 0;
    (void)webui_kv_int(text, n, "fan_min_c", &min_c);
    (void)webui_kv_int(text, n, "fan_max_c", &max_c);
    if (webui_kv_int(text, n, "fan_restores", &restores) != 1 || restores < 0) restores = 0;
    if (webui_kv_str(text, n, "fan_guard", guard, sizeof guard) != 1 ||
        (strcmp(guard, "ok") != 0 && strcmp(guard, "failing") != 0))
        strcpy(guard, "off");
    k = snprintf(out, cap, "{\"cpu_c\":");
    at = k < 0 ? cap : (size_t)k;
    if (at < cap) {
        k = cpu ? snprintf(out + at, cap - at, "%ld", cpu) : snprintf(out + at, cap - at, "null");
        at = k < 0 ? cap : at + (size_t)k;
    }
    if (at < cap) {
        k = soc ? snprintf(out + at, cap - at, ",\"soc_c\":%ld", soc) : snprintf(out + at, cap - at, ",\"soc_c\":null");
        at = k < 0 ? cap : at + (size_t)k;
    }
    /* key + 4 drops the "fan_" prefix: fan_duty_pct -> duty_pct */
    at = fan_put_int(out, cap, at, text, n, "fan_duty_pct", -1);
    at = fan_put_int(out, cap, at, text, n, "fan_target_c", -1);
    at = fan_put_int(out, cap, at, text, n, "fan_stock_c", 0);
    at = fan_put_int(out, cap, at, text, n, "fan_last_restore_s", -1);
    if (at < cap) {
        k = snprintf(out + at, cap - at, ",\"pinned_c\":%ld,\"min_c\":%ld,\"max_c\":%ld,\"restores\":%ld,\"guard\":\"%s\"",
                     pinned, min_c, max_c, restores, guard);
        at = k < 0 ? cap : at + (size_t)k;
    }
    /* one decimal as the helper wrote it ("47.3"); anything else is not passed through */
    {
        int ok = webui_kv_str(text, n, "fan_filtered_c", filtered, sizeof filtered) == 1 && filtered[0] >= '1' &&
                 filtered[0] <= '9';
        for (size_t i = 0; ok && filtered[i]; i++)
            if (!((filtered[i] >= '0' && filtered[i] <= '9') || (filtered[i] == '.' && i > 0 && filtered[i + 1])))
                ok = 0;
        if (at < cap) {
            k = ok ? snprintf(out + at, cap - at, ",\"filtered_c\":%s}", filtered)
                   : snprintf(out + at, cap - at, ",\"filtered_c\":null}");
            at = k < 0 ? cap : at + (size_t)k;
        }
    }
    return at >= cap ? -1 : (int)at;
}

int webui_is_game_title_id(const char *tid) {
    /* PS5 (PPSA, and PCSx/PLxS... for older regional ranges), PS4 (CUSA, PCJS/PLJS Japan, PCAS/PLAS
     * Asia, PCKS/PLKS Korea, ECAS/ELAS Asia) and the PS Vita/PS3-era PCSA..PCSH ranges. */
    static const char *const k_prefixes[] = { "PPSA", "CUSA", "PCJS", "PCAS", "PLJS", "PLAS", "ECAS", "ELAS", "PCKS",
                                              "PLKS", "PCSA", "PCSB", "PCSC", "PCSD", "PCSE", "PCSF", "PCSG", "PCSH" };
    size_t i;
    if (!tid || strlen(tid) != 9 || strcmp(tid, WEBUI_TILE_TITLE_ID) == 0) return 0;
    for (i = 4; i < 9; i++)
        if (tid[i] < '0' || tid[i] > '9') return 0;
    for (i = 0; i < sizeof k_prefixes / sizeof k_prefixes[0]; i++)
        if (memcmp(tid, k_prefixes[i], 4) == 0) return 1;
    return 0;
}

int webui_focus_running(const char *json, size_t n, char *title_id, size_t cap, uint32_t *app_id) {
    const char *e = json + n, *p = top_value(json, n, "apps");
    long long focus = -1, big = -1;
    int pass;
    if (cap) title_id[0] = '\0';
    if (!p || *p != '[') return -1;
    (void)json_i64(json, n, "focus_app_id", &focus);
    (void)json_i64(json, n, "big_app_id", &big);
    /* pass 0: the app on screen (focus flag, else big app); pass 1: the first GAME (never the tile,
     * the browser or a homebrew app: Close would kill it) */
    for (pass = 0; pass < 2; pass++) {
        long long want = pass == 0 ? (focus > 0 ? focus : big) : -1;
        const char *q = p + 1;
        if (pass == 0 && want <= 0) continue;
        for (;;) {
            const char *el, *end;
            char tid[32];
            uint64_t aid = 0;
            q = ws(q, e);
            if (q >= e) return -1;
            if (*q == ']') break;
            el = q;
            end = skip_val(q, e);
            if (!end) return -1;
            if (*el == '{' && webui_json_str(el, (size_t)(end - el), "title_id", tid, sizeof tid) == 1 && tid[0] &&
                strncmp(tid, "NPXS", 4) != 0 && strcmp(tid, WEBUI_TILE_TITLE_ID) != 0 &&
                (pass == 0 || webui_is_game_title_id(tid)) &&
                webui_json_u64(el, (size_t)(end - el), "app_id", &aid) == 1 &&
                (pass == 1 || (long long)aid == want) && strlen(tid) < cap) {
                memcpy(title_id, tid, strlen(tid) + 1);
                *app_id = (uint32_t)aid;
                return 1;
            }
            q = ws(end, e);
            if (q < e && *q == ',') q++;
        }
    }
    return 0;
}

/* ---- cheats ---- */

int webui_cheats_title_ok(const char *tid) {
    size_t i, n = tid ? strlen(tid) : 0;
    if (n < 4 || n > 15) return 0;
    for (i = 0; i < n; i++) {
        char c = tid[i];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9'))) return 0;
    }
    return 1;
}

int webui_cheats_status_to_json(const char *json, size_t n, char *out, size_t cap) {
    char tid[32];
    int enabled = 0, running = 0, held = 0, game, k;
    if (cap) out[0] = '\0';
    if (webui_json_bool(json, n, "enabled", &enabled) != 1) return -1;
    if (webui_json_bool(json, n, "game_running", &running) != 1) running = 0;
    if (webui_json_bool(json, n, "held", &held) != 1) held = 0; /* helpers before 9.5.0 */
    game = running && webui_json_str(json, n, "game_title_id", tid, sizeof tid) == 1 && webui_cheats_title_ok(tid) &&
           strcmp(tid, WEBUI_TILE_TITLE_ID) != 0;
    if (game)
        k = snprintf(out, cap, "{\"engine\":%s,\"game\":{\"title_id\":\"%s\"%s}}", enabled ? "true" : "false", tid,
                     held ? ",\"held\":true" : "");
    else
        k = snprintf(out, cap, "{\"engine\":%s,\"game\":null}", enabled ? "true" : "false");
    if (k < 0 || (size_t)k >= cap) {
        if (cap) out[0] = '\0';
        return -1;
    }
    return 0;
}

/* Room kept for the list's tail: `],"truncated":false}` and the NUL. */
#define CHEATS_TAIL_ROOM 24u
#define CHEATS_NO_FILES "no cheat files found" /* cheats.c cheats_list_mods: an empty list */

int webui_cheats_mods_to_json(const char *json, size_t n, char *out, size_t cap, char *err, size_t err_cap) {
    const char *e = json + n, *p, *q;
    char msg[160], name[512], desc[512], type[32], st[16], why[160];
    size_t pos;
    int written = 0, truncated = 0;
    if (cap) out[0] = '\0';
    if (err_cap) err[0] = '\0';
    if (webui_json_str(json, n, "error", msg, sizeof msg) == 1 && strcmp(msg, CHEATS_NO_FILES) != 0) {
        if (err_cap) {
            size_t l = strlen(msg) < err_cap - 1 ? strlen(msg) : err_cap - 1;
            memcpy(err, msg, l);
            err[l] = '\0';
        }
        return -1;
    }
    p = top_value(json, n, "mods");
    if (!p || *p != '[') return -1;
    if (cap < sizeof "{\"mods\":[" - 1 + CHEATS_TAIL_ROOM) return -1;
    memcpy(out, "{\"mods\":[", 9);
    pos = 9;
    q = p + 1;
    for (;;) {
        const char *el, *end;
        size_t l;
        uint64_t idx;
        int on = 0, other = 0;
        q = ws(q, e);
        if (q >= e) {
            truncated = 1; /* cut before the closing ']' */
            break;
        }
        if (*q == ']') break;
        el = q;
        end = skip_val(q, e);
        if (!end) {
            truncated = 1; /* cut mid-entry */
            break;
        }
        l = (size_t)(end - el);
        if (*el == '{' && webui_json_u64(el, l, "index", &idx) == 1 && idx <= 0x7fffffffu &&
            webui_json_str(el, l, "name", name, sizeof name) == 1 && name[0]) {
            jw_t w;
            char head[48];
            int hl;
            if (webui_json_str(el, l, "desc", desc, sizeof desc) != 1) desc[0] = '\0';
            if (webui_json_str(el, l, "type", type, sizeof type) != 1) type[0] = '\0';
            if (webui_json_bool(el, l, "on", &on) != 1) on = 0;
            if (webui_json_bool(el, l, "other_version", &other) != 1) other = 0; /* XML patch (#527) */
            /* what happened to it in the running game (#627): one of four words, and why */
            if (webui_json_str(el, l, "applied", st, sizeof st) != 1 ||
                (strcmp(st, "applied") != 0 && strcmp(st, "waiting") != 0 && strcmp(st, "failed") != 0 &&
                 strcmp(st, "changed") != 0))
                st[0] = '\0';
            if (!st[0] || webui_json_str(el, l, "applied_reason", why, sizeof why) != 1) why[0] = '\0';
            memset(&w, 0, sizeof w);
            w.out = out + pos;
            w.cap = cap - pos - CHEATS_TAIL_ROOM + 1; /* pos + entry stays CHEATS_TAIL_ROOM short of cap */
            hl = snprintf(head, sizeof head, "%s{\"index\":%llu,\"name\":\"", written ? "," : "", (unsigned long long)idx);
            put(&w, head, (size_t)hl);
            esc(&w, name, strlen(name));
            put(&w, "\",\"desc\":\"", 10);
            esc(&w, desc, strlen(desc));
            put(&w, strcmp(type, "button") == 0 ? "\",\"button\":true" : "\",\"button\":false", strcmp(type, "button") == 0 ? 15 : 16);
            if (other) put(&w, ",\"other_version\":true", 21);
            if (st[0]) {
                put(&w, ",\"applied\":\"", 12);
                put(&w, st, strlen(st));
                put(&w, "\"", 1);
                if (why[0]) {
                    put(&w, ",\"applied_reason\":\"", 19);
                    esc(&w, why, strlen(why));
                    put(&w, "\"", 1);
                }
            }
            put(&w, on ? ",\"on\":true}" : ",\"on\":false}", on ? 11 : 12);
            if (w.full) {
                truncated = 1;
                break;
            }
            pos += w.wrote;
            written++;
        }
        q = ws(end, e);
        if (q < e && *q == ',') q++;
    }
    snprintf(out + pos, cap - pos, "],\"truncated\":%s}", truncated ? "true" : "false");
    return written;
}

const char *webui_cheats_toggle_error(const char *cause, size_t n) {
    char msg[300];
    if (!cause || !n || webui_json_str(cause, n, "err", msg, sizeof msg) != 1) return "toggle_failed";
    if (strstr(msg, "no game is currently running") || strstr(msg, "does not match cheat target"))
        return "game_not_running";
    if (strstr(msg, "no cheat file")) return "no_cheats";
    if (strstr(msg, "index out of range")) return "bad_index";
    if (strstr(msg, "is not loaded in the target process")) return "module_missing";
    if (strstr(msg, "is attached to the game")) return "game_busy";
    /* XML game patches (#527): made for another version or executable of the game */
    if (strstr(msg, "this patch is for") || strstr(msg, "the game's version cannot be read") ||
        strstr(msg, "pattern is not in the game"))
        return "wrong_version";
    return "toggle_failed";
}

static int starts(const char *p, const char *pre) { return strncmp(p, pre, strlen(pre)) == 0; }

const char *webui_unmount_refusal(const char *p, int game_running) {
    if (!p || strcmp(p, "/") == 0 || strcmp(p, "/data") == 0 || starts(p, "/data/") || strcmp(p, "/user") == 0 ||
        starts(p, "/user/") || starts(p, "/system") || starts(p, "/preinst") || starts(p, "/mnt/usb") ||
        starts(p, "/mnt/ext") || starts(p, "/dev"))
        return "system_volume";
    return game_running ? "game_running" : NULL;
}

/* ---- games and processes ---- */

const char *webui_app_refusal(const char *tid, int destructive) {
    size_t i, n = sizeof WEBUI_TILE_TITLE_ID - 1;
    int tile;
    if (!webui_cheats_title_ok(tid)) return "bad_title_id";
    tile = strlen(tid) == n;
    for (i = 0; tile && i < n; i++) {
        char c = tid[i];
        if (c >= 'a' && c <= 'z') c = (char)(c - 'a' + 'A');
        if (c != WEBUI_TILE_TITLE_ID[i]) tile = 0;
    }
    if (tile) return "own_tile";
    if (destructive && (tid[0] == 'N' || tid[0] == 'n') && (tid[1] == 'P' || tid[1] == 'p') &&
        (tid[2] == 'X' || tid[2] == 'x') && (tid[3] == 'S' || tid[3] == 's'))
        return "system_title";
    return NULL;
}

/* Calls fn(element, its length, ctx) for every object in PROCESS_LIST's "procs" array until fn
 * returns non-zero (that value is returned). 0 when the walk ended, -1 when the reply does not parse. */
static int each_process(const char *json, size_t n, int (*fn)(const char *el, size_t len, void *ctx), void *ctx) {
    const char *arr, *q, *e;
    size_t alen;
    if (webui_json_array(json, n, "procs", &arr, &alen) != 1) return -1;
    e = arr + alen;
    q = arr + 1;
    for (;;) {
        const char *el, *end;
        int rc;
        q = ws(q, e);
        if (q >= e) return -1;
        if (*q == ']') return 0;
        el = q;
        end = skip_val(q, e);
        if (!end) return -1;
        if (*el == '{') {
            rc = fn(el, (size_t)(end - el), ctx);
            if (rc) return rc;
        }
        q = ws(end, e);
        if (q < e && *q == ',') q++;
    }
}

struct kill_find {
    uint64_t pid;
    const char *why;
};

static int kill_find_fn(const char *el, size_t len, void *ctx) {
    struct kill_find *k = (struct kill_find *)ctx;
    uint64_t pid;
    char kind[16];
    int self = 0;
    if (webui_json_u64(el, len, "pid", &pid) != 1 || pid != k->pid) return 0;
    if (webui_json_bool(el, len, "is_self", &self) == 1 && self) k->why = "helper";
    else if (webui_json_str(el, len, "kind", kind, sizeof kind) != 1 ||
             (strcmp(kind, "app") != 0 && strcmp(kind, "payload") != 0))
        k->why = "system";
    else k->why = NULL;
    return 1;
}

const char *webui_proc_kill_refusal(const char *json, size_t n, uint64_t pid) {
    struct kill_find k;
    if (pid <= 1) return "system";
    k.pid = pid;
    k.why = "not_running";
    if (each_process(json, n, kill_find_fn, &k) != 1) return "not_running";
    return k.why;
}

struct game_find {
    const char *tid;
    int other;
};

static int game_find_fn(const char *el, size_t len, void *ctx) {
    struct game_find *g = (struct game_find *)ctx;
    char kind[16], tid[32];
    int same;
    if (webui_json_str(el, len, "kind", kind, sizeof kind) != 1 || strcmp(kind, "app") != 0) return 0;
    if (webui_json_str(el, len, "title_id", tid, sizeof tid) != 1 || !tid[0]) return 0;
    same = strcmp(tid, g->tid) == 0;
    return g->other ? !same : same;
}

int webui_proc_game_running(const char *json, size_t n, const char *tid, int other) {
    struct game_find g;
    g.tid = tid ? tid : "";
    g.other = other;
    return each_process(json, n, game_find_fn, &g);
}

/* ---- search index ---- */

/* 1-255 bytes with no '"', '\\' or control character (the index handlers scan quote to quote). */
static int index_text_ok(const char *s) {
    size_t i, n;
    if (!s) return 0;
    n = strlen(s);
    if (n == 0 || n >= WEBUI_INDEX_ROOT_CAP) return 0;
    for (i = 0; i < n; i++) {
        unsigned char c = (unsigned char)s[i];
        if (c < 0x20 || c == 0x7f || c == '"' || c == '\\') return 0;
    }
    return 1;
}

int webui_index_root_ok(const char *p) { return index_text_ok(p) && p[0] == '/'; }

int webui_index_query_ok(const char *q) { return index_text_ok(q); }

void webui_index_collapse_stars(char *q) {
    char *w = q;
    const char *r = q;
    if (!q) return;
    for (; *r; r++) {
        if (*r == '*' && w > q && w[-1] == '*') continue;
        *w++ = *r;
    }
    *w = '\0';
}

int webui_index_start_body(char *out, size_t cap, const char *const *roots, int n) {
    size_t at = 0;
    int i;
    if (!out || !cap) return -1;
    out[0] = '\0';
    if (!roots || n < 1 || n > WEBUI_INDEX_ROOTS_MAX) return -1;
    for (i = 0; i < n; i++)
        if (!webui_index_root_ok(roots[i])) return -1;
    if (cap < 12) return -1;
    memcpy(out, "{\"roots\":[", 10);
    at = 10;
    for (i = 0; i < n; i++) {
        size_t l = strlen(roots[i]);
        /* `,"<root>"` then `]}` and the NUL */
        if (at + l + 3 + 3 > cap) {
            out[0] = '\0';
            return -1;
        }
        if (i) out[at++] = ',';
        out[at++] = '"';
        memcpy(out + at, roots[i], l);
        at += l;
        out[at++] = '"';
    }
    out[at++] = ']';
    out[at++] = '}';
    out[at] = '\0';
    return (int)at;
}

int webui_index_status_to_json(const char *text, size_t n, const char *const *roots, int nroots, int cancelled,
                               char *out, size_t cap) {
    char phase[16];
    uint64_t files = 0;
    int truncated = 0, i;
    jw_t w;
    char num[48];
    if (cap) out[0] = '\0';
    if (!text || webui_json_str(text, n, "phase", phase, sizeof phase) != 1) return -1;
    if (strcmp(phase, "idle") != 0 && strcmp(phase, "building") != 0 && strcmp(phase, "ready") != 0) return -1;
    if (webui_json_u64(text, n, "files", &files) != 1) files = 0;
    if (webui_json_bool(text, n, "truncated", &truncated) != 1) truncated = 0;
    memset(&w, 0, sizeof w);
    w.out = out;
    w.cap = cap;
    put(&w, "{\"phase\":\"", 10);
    put(&w, phase, strlen(phase));
    snprintf(num, sizeof num, "\",\"files\":%llu", (unsigned long long)files);
    put(&w, num, strlen(num));
    put(&w, truncated ? ",\"truncated\":true" : ",\"truncated\":false", truncated ? 17 : 18);
    put(&w, cancelled ? ",\"cancelled\":true" : ",\"cancelled\":false", cancelled ? 17 : 18);
    if (!roots) {
        put(&w, ",\"roots\":null}", 14);
    } else {
        put(&w, ",\"roots\":[", 10);
        for (i = 0; i < nroots; i++) {
            if (i) put(&w, ",", 1);
            put(&w, "\"", 1);
            esc(&w, roots[i], strlen(roots[i]));
            put(&w, "\"", 1);
        }
        put(&w, "]}", 2);
    }
    if (w.full) {
        if (cap) out[0] = '\0';
        return -1;
    }
    out[w.wrote] = '\0';
    return 0;
}

/* `path` is `under` or below it ("/" and NULL: everything). */
static int path_under(const char *path, const char *under) {
    size_t l;
    if (!under || !under[0] || strcmp(under, "/") == 0) return 1;
    l = strlen(under);
    while (l > 1 && under[l - 1] == '/') l--;
    return strncmp(path, under, l) == 0 && (path[l] == '\0' || path[l] == '/');
}

#define INDEX_RESULTS_TAIL 24 /* `],"truncated":false}` + NUL */

int webui_index_results_to_json(const char *text, size_t n, const char *under, size_t max, size_t asked,
                                int (*hide)(const char *path, void *ctx), void *ctx, char *out, size_t cap) {
    const char *e = text + n, *p, *q;
    char path[1024 + 8];
    size_t pos, seen = 0;
    int written = 0, truncated = 0, said = 0;
    if (cap) out[0] = '\0';
    if (!text) return -1;
    p = top_value(text, n, "results");
    if (!p || *p != '[') return -1;
    if (cap < 12 + INDEX_RESULTS_TAIL) return -1;
    if (webui_json_bool(text, n, "truncated", &said) == 1 && said) truncated = 1;
    memcpy(out, "{\"results\":[", 12);
    pos = 12;
    q = p + 1;
    for (;;) {
        const char *el, *end;
        size_t l;
        uint64_t size;
        q = ws(q, e);
        if (q >= e) {
            truncated = 1;
            break;
        }
        if (*q == ']') break;
        el = q;
        end = skip_val(q, e);
        if (!end) {
            truncated = 1;
            break;
        }
        l = (size_t)(end - el);
        seen++;
        if (*el == '{' && webui_json_str(el, l, "path", path, sizeof path) == 1 && path[0] == '/' &&
            webui_json_u64(el, l, "size", &size) == 1 && path_under(path, under) && !(hide && hide(path, ctx))) {
            jw_t w;
            char tail[40];
            int tl;
            if ((size_t)written >= max) {
                truncated = 1;
                break;
            }
            memset(&w, 0, sizeof w);
            w.out = out + pos;
            w.cap = cap - pos - INDEX_RESULTS_TAIL + 1;
            put(&w, written ? ",{\"path\":\"" : "{\"path\":\"", written ? 10 : 9);
            esc(&w, path, strlen(path));
            tl = snprintf(tail, sizeof tail, "\",\"size\":%llu}", (unsigned long long)size);
            put(&w, tail, (size_t)tl);
            if (w.full) {
                truncated = 1;
                break;
            }
            pos += w.wrote;
            written++;
        }
        q = ws(end, e);
        if (q < e && *q == ',') q++;
    }
    if (asked && seen >= asked) truncated = 1;
    snprintf(out + pos, cap - pos, "],\"truncated\":%s}", truncated ? "true" : "false");
    return written;
}
