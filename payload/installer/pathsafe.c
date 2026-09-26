#include "pathsafe.h"

#include <string.h>
#include <stdio.h>

/* True if the path contains a ".." component (bounded by '/' or the ends). */
static int has_dotdot_component(const char *s) {
    const char *p = s;
    while (*p) {
        if (p[0] == '.' && p[1] == '.' &&
            (p[2] == '/' || p[2] == '\0') &&
            (p == s || p[-1] == '/')) {
            return 1;
        }
        p++;
    }
    return 0;
}

/* True if `s` begins with "/mnt/usb" or "/mnt/ext" followed by >=1 digit and
 * then a '/'. */
static int under_mnt_dev(const char *s) {
    const char *tail;
    if (strncmp(s, "/mnt/usb", 8) == 0) tail = s + 8;
    else if (strncmp(s, "/mnt/ext", 8) == 0) tail = s + 8;
    else return 0;
    if (*tail < '0' || *tail > '9') return 0;
    while (*tail >= '0' && *tail <= '9') tail++;
    return *tail == '/';
}

static int ends_with_pkg(const char *s, size_t n) {
    if (n < 4) return 0;
    const char *e = s + n - 4;
    return e[0] == '.' &&
           (e[1] == 'p' || e[1] == 'P') &&
           (e[2] == 'k' || e[2] == 'K') &&
           (e[3] == 'g' || e[3] == 'G');
}

int inst_path_is_safe(const char *path) {
    if (path == NULL || path[0] != '/') return 0;
    size_t n = strlen(path);
    if (n == 0 || n >= INST_PATH_MAX_SAFE) return 0;
    if (path[n - 1] == '/') return 0;          /* no trailing slash */
    if (has_dotdot_component(path)) return 0;
    if (!ends_with_pkg(path, n)) return 0;
    if (strncmp(path, "/data/", 6) == 0) return 1;
    if (strncmp(path, "/user/", 6) == 0) return 1;
    if (under_mnt_dev(path)) return 1;
    return 0;
}

int inst_rewrite_path(const char *in, char *out, size_t cap) {
    int n;
    if (strncmp(in, "/data/", 6) == 0) {
        n = snprintf(out, cap, "/user%s", in);
    } else {
        n = snprintf(out, cap, "%s", in);
    }
    if (n < 0 || (size_t)n >= cap) return -1;
    return 0;
}

const char *inst_basename(const char *path) {
    const char *slash = strrchr(path, '/');
    return slash ? slash + 1 : path;
}

static int is_unreserved(unsigned char c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
           (c >= '0' && c <= '9') ||
           c == '-' || c == '_' || c == '.' || c == '~';
}

int inst_percent_encode(const char *in, char *out, size_t cap) {
    static const char hex[] = "0123456789ABCDEF";
    size_t o = 0;
    for (const unsigned char *p = (const unsigned char *)in; *p; p++) {
        if (is_unreserved(*p)) {
            if (o + 1 >= cap) return -1;
            out[o++] = (char)*p;
        } else {
            if (o + 3 >= cap) return -1;
            out[o++] = '%';
            out[o++] = hex[*p >> 4];
            out[o++] = hex[*p & 0xF];
        }
    }
    if (o >= cap) return -1;
    out[o] = '\0';
    return 0;
}

int inst_build_loopback_url(char *out, size_t cap, uint16_t port,
                            const char *attempt, const char *base) {
    char enc[INST_PATH_MAX_SAFE * 3];
    if (inst_percent_encode(base, enc, sizeof(enc)) != 0) return -1;
    int n = snprintf(out, cap, "http://127.0.0.1:%u/%s/%s",
                     (unsigned)port, attempt, enc);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

int inst_is_network_class(uint32_t code) {
    return (code & 0xFFFF0000u) == 0x80430000u ? 1 : 0;
}
