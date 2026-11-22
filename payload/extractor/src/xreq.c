/* The helper's request (a flat JSON object) and the status the extractor writes back. */
#include "xreq.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* The value at top-level key `key`: a string (unescaped into out) or a bare literal (true, 12).
 * 1 found, 0 absent, -1 malformed or too long. Only flat objects: the request never nests. */
static int json_get(const char *s, const char *key, char *out, size_t cap, int *is_str) {
    size_t klen = strlen(key);
    const char *p = s;
    while ((p = strchr(p, '"')) != NULL) {
        const char *k = p + 1, *q = k;
        while (*q && *q != '"') {
            if (*q == '\\' && q[1]) q++;
            q++;
        }
        if (!*q) return 0;
        {
            const char *after = q + 1;
            while (*after == ' ' || *after == '\t' || *after == '\n' || *after == '\r') after++;
            if (*after != ':') {
                p = q + 1;
                continue;
            }
            if ((size_t)(q - k) != klen || memcmp(k, key, klen) != 0) {
                /* Skip this key's value so a string value is never read as a key. */
                after++;
                while (*after == ' ' || *after == '\t' || *after == '\n' || *after == '\r') after++;
                if (*after == '"') {
                    after++;
                    while (*after && *after != '"') {
                        if (*after == '\\' && after[1]) after++;
                        after++;
                    }
                    if (!*after) return 0;
                    p = after + 1;
                } else {
                    p = after;
                }
                continue;
            }
            after++;
            while (*after == ' ' || *after == '\t' || *after == '\n' || *after == '\r') after++;
            if (*after == '"') {
                size_t at = 0;
                const char *v = after + 1;
                *is_str = 1;
                while (*v && *v != '"') {
                    unsigned cp;
                    if (*v == '\\') {
                        v++;
                        switch (*v) {
                        case 'n': cp = '\n'; break;
                        case 't': cp = '\t'; break;
                        case 'r': cp = '\r'; break;
                        case 'b': cp = '\b'; break;
                        case 'f': cp = '\f'; break;
                        case 'u': {
                            char hex[5] = {0};
                            if (strlen(v + 1) < 4) return -1;
                            memcpy(hex, v + 1, 4);
                            cp = (unsigned)strtoul(hex, NULL, 16);
                            v += 4;
                            if (cp >= 0xD800 && cp <= 0xDBFF && v[1] == '\\' && v[2] == 'u') {
                                unsigned lo;
                                memcpy(hex, v + 3, 4);
                                lo = (unsigned)strtoul(hex, NULL, 16);
                                if (lo >= 0xDC00 && lo <= 0xDFFF) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    v += 6;
                                }
                            }
                            break;
                        }
                        case 0: return -1;
                        default: cp = (unsigned char)*v; break;
                        }
                        v++;
                    } else {
                        cp = (unsigned char)*v++;
                        if (at + 1 >= cap) return -1;
                        out[at++] = (char)cp;
                        continue;
                    }
                    /* UTF-8 out. */
                    if (cp == 0) return -1;
                    if (at + 5 >= cap) return -1;
                    if (cp < 0x80) out[at++] = (char)cp;
                    else if (cp < 0x800) {
                        out[at++] = (char)(0xC0 | (cp >> 6));
                        out[at++] = (char)(0x80 | (cp & 0x3F));
                    } else if (cp < 0x10000) {
                        out[at++] = (char)(0xE0 | (cp >> 12));
                        out[at++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                        out[at++] = (char)(0x80 | (cp & 0x3F));
                    } else {
                        out[at++] = (char)(0xF0 | (cp >> 18));
                        out[at++] = (char)(0x80 | ((cp >> 12) & 0x3F));
                        out[at++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                        out[at++] = (char)(0x80 | (cp & 0x3F));
                    }
                }
                if (*v != '"') return -1;
                out[at] = 0;
                return 1;
            }
            {
                size_t at = 0;
                *is_str = 0;
                while (*after && *after != ',' && *after != '}' && *after != ' ' && *after != '\n') {
                    if (at + 1 >= cap) return -1;
                    out[at++] = *after++;
                }
                out[at] = 0;
                return at ? 1 : -1;
            }
        }
    }
    return 0;
}

static int get_str(const char *s, const char *key, char *out, size_t cap) {
    int is_str = 0, rc = json_get(s, key, out, cap, &is_str);
    if (rc == 1 && !is_str) return -1;
    return rc;
}

static int get_bool(const char *s, const char *key, int *v) {
    char b[8];
    int is_str = 0, rc = json_get(s, key, b, sizeof b, &is_str);
    if (rc <= 0) return rc;
    if (is_str) return -1;
    if (strcmp(b, "true") == 0) *v = 1;
    else if (strcmp(b, "false") == 0) *v = 0;
    else return -1;
    return 1;
}

int xreq_parse(const char *json, xjob_t *j) {
    int rc;
    if (get_str(json, "id", j->id, sizeof j->id) < 0) return -1;
    if (get_str(json, "archive", j->archive, sizeof j->archive) != 1 || j->archive[0] != '/') return -1;
    if (get_str(json, "dest", j->dest, sizeof j->dest) != 1 || j->dest[0] != '/') return -1;
    rc = get_str(json, "password", j->password, sizeof j->password);
    if (rc < 0) return -1;
    j->has_password = rc == 1;
    if (get_bool(json, "merge", &j->merge) < 0) return -1;
    if (get_bool(json, "replace", &j->replace) < 0) return -1;
    if (get_bool(json, "list", &j->list) < 0) return -1;
    if (get_str(json, "top", j->top_map, sizeof j->top_map) < 0) return -1;
    return 0;
}

int xreq_read_file(const char *path, xjob_t *j) {
    char *buf;
    ssize_t n;
    size_t at = 0, cap = 65536;
    int fd = open(path, O_RDONLY), rc;
    if (fd < 0) return -1;
    buf = malloc(cap);
    if (!buf) {
        close(fd);
        return -1;
    }
    while ((n = read(fd, buf + at, cap - 1 - at)) > 0) {
        at += (size_t)n;
        if (at + 1 >= cap) break;
    }
    close(fd);
    buf[at] = 0;
    rc = n < 0 ? -1 : xreq_parse(buf, j);
    /* The password is not kept anywhere once read. */
    memset(buf, 0, cap);
    free(buf);
    return rc;
}

/* JSON string text of s (quotes included) into out at *at; bytes >= 0x80 pass through (UTF-8). */
static void put_str(char *out, size_t cap, size_t *at, const char *s) {
    if (*at + 2 >= cap) return;
    out[(*at)++] = '"';
    for (; *s && *at + 8 < cap; s++) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\') {
            out[(*at)++] = '\\';
            out[(*at)++] = (char)c;
        } else if (c < 0x20) {
            *at += (size_t)snprintf(out + *at, cap - *at, "\\u%04x", c);
        } else {
            out[(*at)++] = (char)c;
        }
    }
    out[(*at)++] = '"';
    out[*at] = 0;
}

int xstatus_format(char *out, size_t cap, const xjob_t *j, const char *state, long pid) {
    size_t at = 0;
    at += (size_t)snprintf(out, cap, "{\"id\":");
    put_str(out, cap, &at, j->id);
    if (at >= cap) return -1;
    at += (size_t)snprintf(out + at, cap - at, ",\"pid\":%ld,\"state\":\"%s\",\"files_done\":%llu,\"files_total\":%llu,"
                           "\"bytes_done\":%llu,\"bytes_total\":%llu,\"files\":%llu,\"dirs\":%llu,\"bytes\":%llu,"
                           "\"skipped\":%llu,\"unsafe\":%llu,\"links\":%llu,\"created_dest\":%s,\"current\":",
                           pid, state, (unsigned long long)j->files_done, (unsigned long long)j->files_total,
                           (unsigned long long)j->bytes_done, (unsigned long long)j->bytes_total, (unsigned long long)j->files,
                           (unsigned long long)j->dirs, (unsigned long long)j->bytes, (unsigned long long)j->skipped,
                           (unsigned long long)j->unsafe, (unsigned long long)j->links, j->created_dest ? "true" : "false");
    if (at >= cap) return -1;
    put_str(out, cap, &at, j->current);
    if (at + 12 >= cap) return -1;
    at += (size_t)snprintf(out + at, cap - at, ",\"message\":");
    put_str(out, cap, &at, j->cause);
    if (j->list) {
        if (at + 12 >= cap) return -1;
        at += (size_t)snprintf(out + at, cap - at, ",\"top\":");
        put_str(out, cap, &at, j->top);
        if (at + 16 >= cap) return -1;
        at += (size_t)snprintf(out + at, cap - at, ",\"top_dirs\":");
        put_str(out, cap, &at, j->top_dirs);
        if (at + 20 >= cap) return -1;
        at += (size_t)snprintf(out + at, cap - at, ",\"top_more\":%s", j->top_more ? "true" : "false");
    }
    if (at + 2 >= cap) return -1;
    out[at++] = '}';
    out[at++] = '\n';
    out[at] = 0;
    return (int)at;
}

static int xstatus_put(const char *path, char *tmp, size_t tcap, const char *buf, int len);

int xstatus_write(const char *path, const xjob_t *j, const char *state, long pid) {
    /* A listing's names take up to XJ_TOP bytes, escaped up to six times over. On the heap: the
     * feeder thread of a compressed tar writes the status too. */
    char small[4096], *buf = small, tmp[XJ_PATH + 8];
    size_t cap = sizeof small;
    int len, rc;
    if (j->list) {
        cap = 4096 + 12 * (size_t)XJ_TOP;
        buf = malloc(cap);
        if (!buf) return -1;
    }
    len = xstatus_format(buf, cap, j, state, pid);
    rc = len <= 0 ? -1 : xstatus_put(path, tmp, sizeof tmp, buf, len);
    if (buf != small) free(buf);
    return rc;
}

static int xstatus_put(const char *path, char *tmp, size_t tcap, const char *buf, int len) {
    int fd;
    snprintf(tmp, tcap, "%s.tmp", path);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    if (write(fd, buf, (size_t)len) != len) {
        close(fd);
        unlink(tmp);
        return -1;
    }
    close(fd);
    /* Same folder: never across a mount. */
    return rename(tmp, path);
}
