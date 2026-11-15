/* The formats of the files the helper and ps5upload-convert exchange (console_convert.h): pure
 * text in, text out, so the host test (payload/tests/console_convert_selftest.c) covers every
 * line. */
#include "console_convert.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "webui_codec.h"

/* Appends `"key":"<s escaped>"` (with a leading comma when `comma`). 0, or -1 when it does not fit. */
static int put_member(char *out, size_t cap, size_t *at, int comma, const char *key, const char *s) {
    int w = snprintf(out + *at, cap - *at, "%s\"%s\":\"", comma ? "," : "", key);
    size_t need;
    if (w < 0 || (size_t)w >= cap - *at) return -1;
    *at += (size_t)w;
    need = webui_json_escape(out + *at, cap - *at, s, strlen(s));
    if (need + 2 >= cap - *at) return -1;
    *at += need;
    out[(*at)++] = '"';
    out[*at] = 0;
    return 0;
}

int cc_request_json(char *out, size_t cap, const char *id, const char *op, const char *path, const cc_build_t *b) {
    size_t at = 0;
    int w;
    if (cap < 2) return -1;
    out[at++] = '{';
    out[at] = 0;
    if (put_member(out, cap, &at, 0, "id", id) != 0 || put_member(out, cap, &at, 1, "op", op) != 0 ||
        put_member(out, cap, &at, 1, "path", path) != 0)
        return -1;
    if (b) {
        if (put_member(out, cap, &at, 1, "dest", b->dest) != 0 || put_member(out, cap, &at, 1, "format", b->format) != 0)
            return -1;
        w = snprintf(out + at, cap - at, ",\"zstd\":%s,\"verify\":\"%s\"", b->zstd ? "true" : "false",
                     b->full_verify ? "full" : "fast");
        if (w < 0 || (size_t)w >= cap - at) return -1;
        at += (size_t)w;
        if (b->zstd && b->zstd_level > 0) {
            w = snprintf(out + at, cap - at, ",\"zstd_level\":%d", b->zstd_level);
            if (w < 0 || (size_t)w >= cap - at) return -1;
            at += (size_t)w;
        }
        if (b->zstd && b->zstd_runtime && b->zstd_runtime[0] &&
            put_member(out, cap, &at, 1, "zstd_runtime", b->zstd_runtime) != 0)
            return -1;
    }
    if (at + 3 > cap) return -1;
    out[at++] = '}';
    out[at++] = '\n';
    out[at] = 0;
    return (int)at;
}

static int is_space(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r'; }

/* The end of the string that opens at buf[i] (its closing quote's index), or n when it is cut. */
static size_t string_end(const char *buf, size_t n, size_t i) {
    for (i++; i < n; i++) {
        if (buf[i] == '\\') i++;
        else if (buf[i] == '"') return i;
    }
    return n;
}

/* The end (one past the closing bracket) of the object or array that opens at buf[i], or 0 when
 * it is cut short or its brackets do not pair up. */
static size_t value_end(const char *buf, size_t n, size_t i) {
    int depth = 0;
    for (; i < n; i++) {
        char c = buf[i];
        if (c == '"') {
            i = string_end(buf, n, i);
            if (i >= n) return 0;
        } else if (c == '{' || c == '[') {
            depth++;
        } else if (c == '}' || c == ']') {
            if (--depth == 0) return i + 1;
            if (depth < 0) return 0;
        }
    }
    return 0;
}

/* The object value of top-level `key` in the complete object buf[0..n): its span, or 0. */
static int object_member(const char *buf, size_t n, const char *key, const char **val, size_t *len) {
    size_t klen = strlen(key), i = 1;
    while (i < n) {
        char c = buf[i];
        if (c == '"') {
            size_t e = string_end(buf, n, i), v;
            if (e >= n) return 0;
            v = e + 1;
            while (v < n && is_space(buf[v])) v++;
            if (v < n && buf[v] == ':') {
                /* A key: its value follows. */
                int ours = e - i - 1 == klen && memcmp(buf + i + 1, key, klen) == 0;
                v++;
                while (v < n && is_space(buf[v])) v++;
                if (v < n && (buf[v] == '{' || buf[v] == '[')) {
                    size_t end = value_end(buf, n, v);
                    if (!end) return 0;
                    if (ours && buf[v] == '{') {
                        *val = buf + v;
                        *len = end - v;
                        return 1;
                    }
                    i = end;
                    continue;
                }
                if (ours) return 0; /* there, but not an object */
                i = v;
                continue;
            }
            i = e + 1; /* a string value */
            continue;
        }
        i++;
    }
    return 0;
}

int cc_status_parse(const char *buf, size_t n, const char *id, cc_status_t *s) {
    char got[64];
    size_t start = 0;
    memset(s, 0, sizeof *s);
    while (n && is_space(buf[n - 1])) n--;
    while (start < n && is_space(buf[start])) start++;
    buf += start;
    n -= start;
    /* One whole object and nothing else: a file cut anywhere fails here, before any field of it
     * is believed. */
    if (n < 2 || buf[0] != '{' || memchr(buf, 0, n) || value_end(buf, n, 0) != n) return 0;
    if (webui_json_str(buf, n, "id", got, sizeof got) != 1 || strcmp(got, id) != 0) return 0;
    if (webui_json_str(buf, n, "state", s->state, sizeof s->state) != 1) return 0;
    if (strcmp(s->state, "running") != 0 && strcmp(s->state, "done") != 0 && strcmp(s->state, "failed") != 0) return 0;
    /* The rest is optional; one that is unusable (too long, not a string) reads as absent. */
    if (webui_json_str(buf, n, "op", s->op, sizeof s->op) != 1) s->op[0] = 0;
    if (webui_json_str(buf, n, "cause", s->cause, sizeof s->cause) != 1) s->cause[0] = 0;
    if (webui_json_str(buf, n, "detail", s->detail, sizeof s->detail) != 1) s->detail[0] = 0;
    if (webui_json_str(buf, n, "stage", s->stage, sizeof s->stage) != 1) s->stage[0] = 0;
    /* A pid that cannot be the converter's (0, init, past a pid_t) reads as none: cast to pid_t,
     * 0xFFFFFFFF would be -1 (every process) and 0x100000001 would be 1 (init). */
    if (webui_json_u64(buf, n, "pid", &s->pid) != 1 || !cc_pid_ok(s->pid)) s->pid = 0;
    if (webui_json_u64(buf, n, "bytes_done", &s->bytes_done) != 1) s->bytes_done = 0;
    if (webui_json_u64(buf, n, "bytes_total", &s->bytes_total) != 1) s->bytes_total = 0;
    if (!object_member(buf, n, "result", &s->result, &s->result_len)) {
        s->result = NULL;
        s->result_len = 0;
    }
    return 1;
}

/* A job id as the helper makes them: hex digits (and nothing that splits a line or a word). */
static int id_ok(const char *id) {
    size_t i;
    for (i = 0; id[i]; i++)
        if (!((id[i] >= '0' && id[i] <= '9') || (id[i] >= 'a' && id[i] <= 'f'))) return 0;
    return i > 0 && i < CC_JOB_ID_MAX;
}

int cc_record_format(char *out, size_t cap, unsigned long long xpid, long hpid, const char *id, const char *dest) {
    int w;
    if (strchr(dest, '\n') || !id_ok(id)) return -1; /* the record is line-based */
    w = snprintf(out, cap, "%llu %ld %s\n%s\n", xpid, hpid, id, dest);
    return w < 0 || (size_t)w >= cap ? -1 : w;
}

int cc_record_parse(const char *buf, unsigned long long *xpid, long *hpid, char *id, size_t id_cap, char *dest,
                    size_t cap) {
    const char *nl = strchr(buf, '\n'), *end;
    char line[32 + CC_JOB_ID_MAX + 8], got[CC_JOB_ID_MAX + 8];
    size_t len;
    int n;
    if (cap) dest[0] = 0;
    if (id_cap) id[0] = 0;
    if (!nl || (size_t)(nl - buf) >= sizeof line) return 0;
    /* The first line alone: a record from before job ids has none, and the id is never read
     * from the line below it. */
    memcpy(line, buf, (size_t)(nl - buf));
    line[nl - buf] = 0;
    got[0] = 0;
    n = sscanf(line, "%llu %ld %47s", xpid, hpid, got);
    if (n < 2) return 0;
    if (n == 3) {
        if (!id_ok(got) || strlen(got) >= id_cap) return 0;
        memcpy(id, got, strlen(got) + 1);
    }
    nl++;
    end = strchr(nl, '\n');
    len = end ? (size_t)(end - nl) : strlen(nl);
    if (len >= cap) return 0;
    memcpy(dest, nl, len);
    dest[len] = 0;
    return 1;
}

const char *cc_image_format(const char *path) {
    static const char *const formats[] = {"exfat", "ffpkg", "ffpfs"};
    const char *slash = strrchr(path, '/'), *dot = strrchr(slash ? slash : path, '.');
    size_t i, k;
    if (!dot || !dot[1]) return NULL;
    for (i = 0; i < sizeof formats / sizeof *formats; i++) {
        const char *f = formats[i];
        for (k = 0; f[k] && dot[1 + k]; k++) {
            char c = dot[1 + k];
            if (c >= 'A' && c <= 'Z') c = (char)(c - 'A' + 'a');
            if (c != f[k]) break;
        }
        if (!f[k] && !dot[1 + k]) return f;
    }
    return NULL;
}

int cc_format_ok(const char *format) {
    return strcmp(format, "exfat") == 0 || strcmp(format, "ffpkg") == 0 || strcmp(format, "ffpfs") == 0 ||
           strcmp(format, "folder") == 0;
}

int cc_path_within(const char *outer, const char *inner) {
    size_t n = strlen(outer);
    return strncmp(outer, inner, n) == 0 && (inner[n] == 0 || inner[n] == '/');
}

/* cc_path_within without case: on an exFAT drive "g.partial" and "G.partial" are one folder. */
static int path_within_nocase(const char *outer, const char *inner) {
    size_t n = strlen(outer);
    return strncasecmp(outer, inner, n) == 0 && (inner[n] == 0 || inner[n] == '/');
}

int cc_build_reads_own_leftovers(const char *src, const char *dest) {
    static const char *const tails[] = {".partial", ".partial.resume", ".partial.stamp"};
    size_t i, dlen = strlen(dest);
    for (i = 0; i < sizeof tails / sizeof *tails; i++) {
        size_t len = dlen + strlen(tails[i]) + 1;
        char *own = malloc(len); /* on the heap: a job thread's stack is small */
        int hit;
        if (!own) return 1; /* cannot tell: refused */
        snprintf(own, len, "%s%s", dest, tails[i]);
        hit = path_within_nocase(own, src) || path_within_nocase(src, own);
        free(own);
        if (hit) return 1;
    }
    return 0;
}

int cc_runtime_folder_version(const char *name, char *out, size_t cap) {
    size_t plen = sizeof CC_RUNTIME_PREFIX - 1, n, i;
    if (strncmp(name, CC_RUNTIME_PREFIX, plen) != 0) return 0;
    name += plen;
    n = strlen(name);
    if (!n || n >= cap) return 0;
    for (i = 0; i < n; i++) {
        char c = name[i];
        if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '.' || c == '-' ||
              c == '_'))
            return 0;
    }
    memcpy(out, name, n + 1);
    return 1;
}

/* One dotted part's number: 0 when it is empty, not all digits, or too large (as the engine's
 * `parse().unwrap_or(0)`). */
static unsigned long long version_part(const char *s, size_t n) {
    unsigned long long v = 0;
    size_t i;
    for (i = 0; i < n; i++) {
        if (s[i] < '0' || s[i] > '9' || v > (~0ULL - 9) / 10) return 0;
        v = v * 10 + (unsigned)(s[i] - '0');
    }
    return v;
}

int cc_version_older(const char *a, const char *b) {
    for (;;) {
        size_t na = strcspn(a, "."), nb = strcspn(b, ".");
        unsigned long long va = version_part(a, na), vb = version_part(b, nb);
        if (va != vb) return va < vb;
        a += na;
        b += nb;
        /* One has no parts left: it is the older when the other still has some. */
        if (!*a || !*b) return !*a && *b;
        a++;
        b++;
    }
}

int cc_result_with_runtime(char *out, size_t cap, const char *result, size_t len, const char *version) {
    size_t at, i;
    int empty = 1, w;
    while (len && is_space(result[len - 1])) len--;
    if (len < 2 || result[0] != '{' || result[len - 1] != '}' || cap < len) return -1;
    for (i = 1; i + 1 < len; i++)
        if (!is_space(result[i])) empty = 0;
    /* Everything but the closing brace, then the member and the brace again. */
    at = len - 1;
    memcpy(out, result, at);
    out[at] = 0;
    if (version) {
        if (put_member(out, cap, &at, !empty, "zstd_runtime", version) != 0) return -1;
        w = snprintf(out + at, cap - at, "}");
    } else {
        w = snprintf(out + at, cap - at, "%s\"zstd_runtime\":null}", empty ? "" : ",");
    }
    if (w < 0 || (size_t)w >= cap - at) return -1;
    return (int)(at + (size_t)w);
}
