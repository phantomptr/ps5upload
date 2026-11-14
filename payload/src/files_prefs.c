/* The console file manager's shared preferences: see include/files_prefs.h. */
#include "files_prefs.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "webui_codec.h"

/* Room kept free of favourites: a recent entry and `last`, each a path whose every byte escapes to two. */
#define PIN_RESERVE (2u * (2u * FILES_PREFS_PATH_MAX + 4u))

/* One load-change-save at a time, whoever asks (the console page, a PC through the engine). */
static pthread_mutex_t g_prefs_mu = PTHREAD_MUTEX_INITIALIZER;

int files_prefs_op_from_name(const char *name) {
    if (!name) return 0;
    if (strcmp(name, "pin") == 0) return FILES_PREFS_PIN;
    if (strcmp(name, "unpin") == 0) return FILES_PREFS_UNPIN;
    if (strcmp(name, "visit") == 0) return FILES_PREFS_VISIT;
    if (strcmp(name, "forget") == 0) return FILES_PREFS_FORGET;
    return 0;
}

/* Bytes of the well-formed UTF-8 character at `s` (1-4), 0 when it is not one: a path that is not
 * UTF-8 is written as U+FFFD in the JSON (6 bytes a byte, past PIN_RESERVE) and never reads back
 * as itself. */
static size_t utf8_len(const unsigned char *s) {
    unsigned char c = s[0];
    size_t n, k;
    unsigned cp;
    if (c < 0x80) return 1;
    if (c >= 0xC2 && c <= 0xDF) n = 2, cp = c & 0x1F;
    else if (c >= 0xE0 && c <= 0xEF) n = 3, cp = c & 0x0F;
    else if (c >= 0xF0 && c <= 0xF4) n = 4, cp = c & 0x07;
    else return 0;
    for (k = 1; k < n; k++) {
        if ((s[k] & 0xC0) != 0x80) return 0;
        cp = (cp << 6) | (s[k] & 0x3F);
    }
    if ((n == 3 && cp < 0x800) || (n == 4 && (cp < 0x10000 || cp > 0x10FFFF)) || (cp >= 0xD800 && cp <= 0xDFFF))
        return 0;
    return n;
}

int files_prefs_clean_path(const char *in, char *out) {
    size_t i, o = 0, seg;
    if (!in || in[0] != '/') return FILES_PREFS_E_BAD_PATH;
    for (i = 0; in[i];) {
        size_t n = utf8_len((const unsigned char *)in + i);
        if (!n) return FILES_PREFS_E_BAD_PATH;
        i += n;
    }
    for (i = 0; in[i]; i++) {
        unsigned char c = (unsigned char)in[i];
        if (c < 0x20 || c == 0x7f) return FILES_PREFS_E_BAD_PATH;
        if (c == '/' && o > 0 && out[o - 1] == '/') continue; /* '//' */
        if (o + 1 >= FILES_PREFS_PATH_MAX) return FILES_PREFS_E_BAD_PATH;
        out[o++] = (char)c;
    }
    if (o > 1 && out[o - 1] == '/') o--;
    out[o] = '\0';
    /* no "." or ".." component: a stored path names one folder, as written */
    for (i = 1; i <= o; i = seg + 1) {
        size_t len;
        for (seg = i; seg < o && out[seg] != '/'; seg++) {}
        len = seg - i;
        if ((len == 1 && out[i] == '.') || (len == 2 && out[i] == '.' && out[i + 1] == '.')) {
            out[0] = '\0';
            return FILES_PREFS_E_BAD_PATH;
        }
    }
    return FILES_PREFS_OK;
}

void files_prefs_clear(files_prefs_t *p) {
    p->nfav = 0;
    p->nrecent = 0;
    p->has_last = 0;
    p->last[0] = '\0';
}

static int find(char (*list)[FILES_PREFS_PATH_MAX], int n, const char *path) {
    int i;
    for (i = 0; i < n; i++)
        if (strcmp(list[i], path) == 0) return i;
    return -1;
}

static void remove_at(char (*list)[FILES_PREFS_PATH_MAX], int *n, int at) {
    if (at < 0 || at >= *n) return;
    if (at + 1 < *n) memmove(list[at], list[at + 1], (size_t)(*n - at - 1) * FILES_PREFS_PATH_MAX);
    (*n)--;
}

/* Reads one list: clean, unique entries, at most max. 0, or -1 (not an array of strings). */
static int parse_list(const char *json, size_t n, const char *key, char (*list)[FILES_PREFS_PATH_MAX], int *count,
                      int max) {
    const char *arr;
    size_t len, pos = 0;
    char raw[FILES_PREFS_PATH_MAX], clean[FILES_PREFS_PATH_MAX];
    int rc = webui_json_array(json, n, key, &arr, &len);
    *count = 0;
    if (rc == 0) return 0;
    if (rc < 0) return -1;
    for (;;) {
        rc = webui_json_array_str(arr, len, &pos, raw, sizeof raw);
        if (rc == 0) return 0;
        if (rc < 0) return -1;
        if (*count >= max) continue; /* the rest is read (it must still parse) and dropped */
        if (files_prefs_clean_path(raw, clean) != FILES_PREFS_OK) continue;
        if (find(list, *count, clean) >= 0) continue;
        memcpy(list[*count], clean, strlen(clean) + 1);
        (*count)++;
    }
}

static int is_ws(char c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n'; }

int files_prefs_parse(const char *json, size_t n, files_prefs_t *p) {
    char raw[FILES_PREFS_PATH_MAX];
    size_t a, b;
    int rc;
    files_prefs_clear(p);
    if (!json) return 1;
    /* One JSON object: the first and last non-blank bytes are its braces (a cut-short file is
     * garbled, even when what it still has would read). */
    for (a = 0; a < n && is_ws(json[a]); a++) {}
    for (b = n; b > a && is_ws(json[b - 1]); b--) {}
    if (b - a < 2 || json[a] != '{' || json[b - 1] != '}') return 1;
    json += a;
    n = b - a;
    if (parse_list(json, n, "favorites", p->fav, &p->nfav, FILES_PREFS_FAV_MAX) != 0 ||
        parse_list(json, n, "recent", p->recent, &p->nrecent, FILES_PREFS_RECENT_MAX) != 0) {
        files_prefs_clear(p);
        return 1;
    }
    rc = webui_json_str(json, n, "last", raw, sizeof raw);
    if (rc == 1 && files_prefs_clean_path(raw, p->last) == FILES_PREFS_OK) p->has_last = 1;
    else p->last[0] = '\0';
    /* "last":null is the absent value; any other value that is not a string is garbage */
    if (rc < 0 && !webui_json_null(json, n, "last")) {
        files_prefs_clear(p);
        return 1;
    }
    return 0;
}

int files_prefs_load(const char *file, files_prefs_t *p) {
    char *buf;
    size_t n = 0;
    int fd, rc;
    files_prefs_clear(p);
    fd = open(file, O_RDONLY);
    if (fd < 0) return 1;
    buf = malloc(FILES_PREFS_FILE_MAX + 1);
    if (!buf) {
        close(fd);
        return 1;
    }
    for (;;) {
        ssize_t r = read(fd, buf + n, FILES_PREFS_FILE_MAX + 1 - n);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) break;
        n += (size_t)r;
        if (n == FILES_PREFS_FILE_MAX + 1) break;
    }
    close(fd);
    rc = n > FILES_PREFS_FILE_MAX ? 1 : files_prefs_parse(buf, n, p);
    free(buf);
    return rc;
}

static void put(char *out, size_t cap, size_t *at, const char *s) {
    size_t k = strlen(s);
    if (out && *at < cap && k < cap - *at) memcpy(out + *at, s, k);
    *at += k;
}

static void put_path(char *out, size_t cap, size_t *at, const char *s) {
    put(out, cap, at, "\"");
    if (out && *at < cap) *at += webui_json_escape(out + *at, cap - *at, s, strlen(s));
    else *at += webui_json_escape(NULL, 0, s, strlen(s));
    put(out, cap, at, "\"");
}

static void put_list(char *out, size_t cap, size_t *at, const char *key, char (*list)[FILES_PREFS_PATH_MAX], int n) {
    int i;
    put(out, cap, at, key);
    put(out, cap, at, ":[");
    for (i = 0; i < n; i++) {
        if (i) put(out, cap, at, ",");
        put_path(out, cap, at, list[i]);
    }
    put(out, cap, at, "]");
}

/* The JSON's full length; written into out as far as it fits (out may be NULL with cap 0). */
static size_t build_json(const files_prefs_t *p, char *out, size_t cap) {
    size_t at = 0;
    files_prefs_t *q = (files_prefs_t *)p; /* the lists are only read */
    put(out, cap, &at, "{\"v\":1,");
    put_list(out, cap, &at, "\"favorites\"", q->fav, p->nfav);
    put(out, cap, &at, ",");
    put_list(out, cap, &at, "\"recent\"", q->recent, p->nrecent);
    put(out, cap, &at, ",\"last\":");
    if (p->has_last) put_path(out, cap, &at, p->last);
    else put(out, cap, &at, "null");
    put(out, cap, &at, "}");
    return at;
}

int files_prefs_to_json(const files_prefs_t *p, char *out, size_t cap) {
    size_t at = build_json(p, out, cap);
    if (at >= cap) {
        if (cap) out[0] = '\0';
        return -1;
    }
    out[at] = '\0';
    return (int)at;
}

static int write_all(const char *tmp, const char *text, size_t len) {
    size_t done = 0;
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    while (done < len) {
        ssize_t w = write(fd, text + done, len - done);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) break;
        done += (size_t)w;
    }
    if (done != len || fsync(fd) != 0) {
        close(fd);
        return -1;
    }
    return close(fd);
}

int files_prefs_save(const char *file, const files_prefs_t *p) {
    char tmp[FILES_PREFS_PATH_MAX + 8];
    char *text;
    int len, k;
    k = snprintf(tmp, sizeof tmp, "%s.tmp", file); /* same directory as `file`: the rename never crosses a mount */
    if (k < 0 || (size_t)k >= sizeof tmp) return FILES_PREFS_E_IO;
    text = malloc(FILES_PREFS_FILE_MAX + 1);
    if (!text) return FILES_PREFS_E_NOMEM;
    len = files_prefs_to_json(p, text, FILES_PREFS_FILE_MAX + 1);
    if (len < 0) {
        free(text);
        return FILES_PREFS_E_FULL;
    }
    if (write_all(tmp, text, (size_t)len) != 0 || rename(tmp, file) != 0) {
        unlink(tmp);
        free(text);
        return FILES_PREFS_E_IO;
    }
    free(text);
    return FILES_PREFS_OK;
}

int files_prefs_pin(files_prefs_t *p, const char *path) {
    char clean[FILES_PREFS_PATH_MAX];
    if (files_prefs_clean_path(path, clean) != FILES_PREFS_OK) return FILES_PREFS_E_BAD_PATH;
    if (find(p->fav, p->nfav, clean) >= 0) return FILES_PREFS_OK;
    if (p->nfav >= FILES_PREFS_FAV_MAX) return FILES_PREFS_E_FULL;
    memcpy(p->fav[p->nfav++], clean, strlen(clean) + 1);
    return FILES_PREFS_OK;
}

int files_prefs_unpin(files_prefs_t *p, const char *path) {
    char clean[FILES_PREFS_PATH_MAX];
    if (files_prefs_clean_path(path, clean) != FILES_PREFS_OK) return FILES_PREFS_E_BAD_PATH;
    remove_at(p->fav, &p->nfav, find(p->fav, p->nfav, clean));
    return FILES_PREFS_OK;
}

/* `a` is a strict ancestor of `b` ("/" of everything else). */
static int is_ancestor(const char *a, const char *b) {
    size_t la = strlen(a);
    if (strcmp(a, "/") == 0) return strcmp(b, "/") != 0;
    return strncmp(a, b, la) == 0 && b[la] == '/';
}

int files_prefs_visit(files_prefs_t *p, const char *path) {
    char clean[FILES_PREFS_PATH_MAX];
    if (files_prefs_clean_path(path, clean) != FILES_PREFS_OK) return FILES_PREFS_E_BAD_PATH;
    memcpy(p->last, clean, strlen(clean) + 1);
    p->has_last = 1;
    remove_at(p->recent, &p->nrecent, find(p->recent, p->nrecent, clean));
    /* walking down: the folder just passed through is replaced by the one reached */
    if (p->nrecent > 0 && is_ancestor(p->recent[0], clean)) remove_at(p->recent, &p->nrecent, 0);
    if (p->nrecent >= FILES_PREFS_RECENT_MAX) p->nrecent = FILES_PREFS_RECENT_MAX - 1;
    memmove(p->recent[1], p->recent[0], (size_t)p->nrecent * FILES_PREFS_PATH_MAX);
    memcpy(p->recent[0], clean, strlen(clean) + 1);
    p->nrecent++;
    return FILES_PREFS_OK;
}

int files_prefs_forget(files_prefs_t *p, const char *path) {
    char clean[FILES_PREFS_PATH_MAX];
    if (files_prefs_clean_path(path, clean) != FILES_PREFS_OK) return FILES_PREFS_E_BAD_PATH;
    remove_at(p->recent, &p->nrecent, find(p->recent, p->nrecent, clean));
    if (p->has_last && strcmp(p->last, clean) == 0) {
        p->has_last = 0;
        p->last[0] = '\0';
    }
    return FILES_PREFS_OK;
}

int files_prefs_get(const char *file, char *out, size_t cap) {
    files_prefs_t *p = malloc(sizeof *p);
    int len;
    if (!p) return FILES_PREFS_E_NOMEM;
    pthread_mutex_lock(&g_prefs_mu);
    (void)files_prefs_load(file, p);
    pthread_mutex_unlock(&g_prefs_mu);
    len = files_prefs_to_json(p, out, cap);
    free(p);
    return len < 0 ? FILES_PREFS_E_FULL : len;
}

/* Already the newest recent folder and the last one: a visit would write the same file again. */
static int visit_is_noop(const files_prefs_t *p, const char *path) {
    char clean[FILES_PREFS_PATH_MAX];
    if (files_prefs_clean_path(path, clean) != FILES_PREFS_OK) return 0;
    return p->has_last && strcmp(p->last, clean) == 0 && p->nrecent > 0 && strcmp(p->recent[0], clean) == 0;
}

int files_prefs_update(const char *file, int op, const char *path, char *out, size_t cap) {
    files_prefs_t *p;
    int rc, len;
    if (op != FILES_PREFS_PIN && op != FILES_PREFS_UNPIN && op != FILES_PREFS_VISIT && op != FILES_PREFS_FORGET)
        return FILES_PREFS_E_BAD_OP;
    p = malloc(sizeof *p);
    if (!p) return FILES_PREFS_E_NOMEM;
    pthread_mutex_lock(&g_prefs_mu);
    (void)files_prefs_load(file, p); /* garbage reads as empty and is replaced below */
    if (op == FILES_PREFS_VISIT && visit_is_noop(p, path)) {
        pthread_mutex_unlock(&g_prefs_mu);
        len = files_prefs_to_json(p, out, cap);
        free(p);
        return len < 0 ? FILES_PREFS_E_FULL : len;
    }
    rc = op == FILES_PREFS_PIN ? files_prefs_pin(p, path)
         : op == FILES_PREFS_UNPIN ? files_prefs_unpin(p, path)
         : op == FILES_PREFS_FORGET ? files_prefs_forget(p, path)
                                    : files_prefs_visit(p, path);
    /* favourites never take the room a visit needs: one recent folder and the last one, at their longest */
    if (rc == FILES_PREFS_OK && op == FILES_PREFS_PIN && build_json(p, NULL, 0) > FILES_PREFS_FILE_MAX - PIN_RESERVE)
        rc = FILES_PREFS_E_FULL;
    if (rc == FILES_PREFS_OK) {
        rc = files_prefs_save(file, p);
        /* a visit makes room by forgetting the oldest recent folders (keeping the one just visited) */
        while (rc == FILES_PREFS_E_FULL && op == FILES_PREFS_VISIT && p->nrecent > 1) {
            p->nrecent--;
            rc = files_prefs_save(file, p);
        }
    }
    pthread_mutex_unlock(&g_prefs_mu);
    if (rc != FILES_PREFS_OK) {
        free(p);
        return rc;
    }
    len = files_prefs_to_json(p, out, cap);
    free(p);
    return len < 0 ? FILES_PREFS_E_FULL : len;
}
