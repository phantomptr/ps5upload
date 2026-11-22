/* Console clean-up rules: see include/cleanup_rules.h. Plain C, no console calls. */
#include "cleanup_rules.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define SCR_PKG_MAGIC 0x7F434E54u     /* "\x7FCNT" */
#define SCR_PKG_MAGIC_FIH 0x7F464948u /* "\x7FFIH": the PS5 container */
#define SCR_ENTRY_SFO 0x1000u
#define SCR_ENTRY_JSON 0x2000u
#define SCR_PIECE_MAX (64u * 1024u)

static uint32_t be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}
static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static uint64_t le64(const uint8_t *p) { return (uint64_t)le32(p) | ((uint64_t)le32(p + 4) << 32); }

int scr_is_title_id(const char *s) {
    if (!s) return 0;
    for (int i = 0; i < 4; i++)
        if (s[i] < 'A' || s[i] > 'Z') return 0;
    for (int i = 4; i < 9; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    return s[9] == '\0';
}

int scr_is_system_title(const char *tid) { return tid && strncmp(tid, "NPXS", 4) == 0; }

/* "UP9000-CUSA07842_00-SCUS974290000001": 6 characters, '-', the title id, '_', 2 digits, '-', label. */
static int cid_shape(const char *cid) {
    if (!cid || strlen(cid) < 20) return 0;
    for (int i = 0; i < 6; i++) {
        char c = cid[i];
        if (!((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9'))) return 0;
    }
    if (cid[6] != '-' || cid[16] != '_' || cid[19] != '-') return 0;
    if (cid[17] < '0' || cid[17] > '9' || cid[18] < '0' || cid[18] > '9') return 0;
    for (const char *p = cid; *p; p++)
        if (*p == '/' || *p == '\\' || (unsigned char)*p < 0x21 || (unsigned char)*p > 0x7E) return 0;
    char tid[10];
    memcpy(tid, cid + 7, 9);
    tid[9] = '\0';
    return scr_is_title_id(tid);
}

int scr_tid_of_content_id(const char *cid, char out[10]) {
    if (!cid_shape(cid)) return 0;
    memcpy(out, cid + 7, 9);
    out[9] = '\0';
    return 1;
}

int scr_label_of_content_id(const char *cid, char out[17]) {
    if (!cid_shape(cid)) return 0;
    size_t n = strlen(cid + 20);
    if (n == 0 || n > 16) return 0;
    memcpy(out, cid + 20, n + 1);
    return 1;
}

int scr_version_cmp(const char *a, const char *b) {
    if (!a || !b || !a[0] || !b[0]) return -2;
    const char *p = a, *q = b;
    for (;;) {
        unsigned long x = 0, y = 0;
        int dx = 0, dy = 0;
        while (*p >= '0' && *p <= '9') {
            x = x * 10u + (unsigned long)(*p - '0');
            p++;
            dx++;
        }
        while (*q >= '0' && *q <= '9') {
            y = y * 10u + (unsigned long)(*q - '0');
            q++;
            dy++;
        }
        if ((*p && *p != '.') || (*q && *q != '.')) return -2;
        if (!dx && *p) return -2;
        if (!dy && *q) return -2;
        if (x != y) return x < y ? -1 : 1;
        if (!*p && !*q) return 0;
        if (*p) p++;
        if (*q) q++;
        if (!*p && !*q) return 0;
    }
}

int scr_sfo_str(const uint8_t *sfo, size_t n, const char *key, char *out, size_t cap) {
    if (!sfo || n < 0x14 || memcmp(sfo, "\0PSF", 4) != 0 || cap == 0) return 0;
    size_t keys = le32(sfo + 8), data = le32(sfo + 12), cnt = le32(sfo + 16);
    if (keys > n || data > n || cnt > 256 || 0x14 + cnt * 0x10 > n) return 0;
    size_t klen = strlen(key);
    for (size_t i = 0; i < cnt; i++) {
        const uint8_t *e = sfo + 0x14 + i * 0x10;
        size_t ko = keys + (size_t)(e[0] | (e[1] << 8));
        size_t len = le32(e + 4);
        size_t d = data + le32(e + 12);
        if (ko + klen + 1 > n || d > n || len > n - d) continue;
        if (memcmp(sfo + ko, key, klen) != 0 || sfo[ko + klen] != 0) continue;
        size_t w = 0;
        while (w < len && w + 1 < cap && sfo[d + w]) {
            out[w] = (char)sfo[d + w];
            w++;
        }
        out[w] = '\0';
        return 1;
    }
    return 0;
}

int scr_json_str(const char *json, size_t n, const char *key, char *out, size_t cap) {
    size_t klen = strlen(key);
    if (!json || cap == 0) return 0;
    for (size_t i = 0; i + klen + 2 <= n; i++) {
        if (json[i] != '"' || memcmp(json + i + 1, key, klen) != 0 || json[i + 1 + klen] != '"') continue;
        size_t j = i + 2 + klen;
        while (j < n && (json[j] == ' ' || json[j] == '\t' || json[j] == '\r' || json[j] == '\n')) j++;
        if (j >= n || json[j] != ':') continue;
        j++;
        while (j < n && (json[j] == ' ' || json[j] == '\t' || json[j] == '\r' || json[j] == '\n')) j++;
        if (j >= n || json[j] != '"') continue;
        j++;
        size_t w = 0;
        while (j < n && json[j] != '"' && json[j] != '\\' && w + 1 < cap) out[w++] = json[j++];
        if (j >= n || json[j] != '"') return 0;
        out[w] = '\0';
        return 1;
    }
    return 0;
}

/* The version from a PARAM.SFO or param.json piece read at `off`. */
static void read_version(scr_read_at_fn read_at, void *ctx, uint64_t off, uint32_t sz, int json, char *out,
                         size_t cap) {
    if (sz == 0 || sz > SCR_PIECE_MAX) return;
    uint8_t *buf = malloc(sz + 1);
    if (!buf) return;
    if (read_at(ctx, off, buf, sz) == 0) {
        buf[sz] = 0;
        if (json) {
            if (!scr_json_str((const char *)buf, sz, "contentVersion", out, cap))
                (void)scr_json_str((const char *)buf, sz, "masterVersion", out, cap);
        } else {
            (void)scr_sfo_str(buf, sz, "APP_VER", out, cap);
        }
    }
    free(buf);
}

int scr_pkg_read(scr_read_at_fn read_at, void *ctx, uint64_t size, scr_pkg_info_t *out) {
    uint8_t head[0xA0], cnt[0xA0];
    uint64_t base = 0;
    memset(out, 0, sizeof *out);
    if (size < sizeof head || read_at(ctx, 0, head, sizeof head) != 0) return -1;
    uint32_t magic = be32(head);
    if (magic == SCR_PKG_MAGIC_FIH) {
        base = le64(head + 0x58);
        if (base == 0 || base > size || size - base < sizeof cnt) return -1;
        if (read_at(ctx, base, cnt, sizeof cnt) != 0 || be32(cnt) != SCR_PKG_MAGIC) return -1;
        out->ps5 = 1;
    } else if (magic == SCR_PKG_MAGIC) {
        memcpy(cnt, head, sizeof cnt);
    } else {
        return -1;
    }
    memcpy(out->content_id, cnt + 0x40, 36);
    out->content_id[36] = '\0';
    for (int i = 0; i < 36; i++)
        if ((unsigned char)out->content_id[i] < 0x20 || (unsigned char)out->content_id[i] > 0x7E) {
            out->content_id[i] = '\0';
            break;
        }
    if (!scr_tid_of_content_id(out->content_id, out->title_id)) return -1;
    out->content_type = be32(cnt + 0x74);
    out->content_flags = be32(cnt + 0x78);
    if (out->content_type == 0x1B || out->content_type == 0x21) out->kind = 'd';
    else if (out->content_flags & 0x40100000u) out->kind = 'p';
    else if (out->content_type == 0x1A || out->content_type == 0x20 || out->content_type == 0x26) out->kind = 'b';

    uint32_t entries = be32(cnt + 0x10), table = be32(cnt + 0x18);
    if (entries == 0 || entries > 1024 || (uint64_t)table + base + (uint64_t)entries * 0x20 > size) return 0;
    size_t tlen = (size_t)entries * 0x20;
    uint8_t *t = malloc(tlen);
    if (!t) return 0;
    if (read_at(ctx, base + table, t, tlen) == 0) {
        uint32_t sfo_off = 0, sfo_sz = 0, js_off = 0, js_sz = 0;
        for (uint32_t i = 0; i < entries; i++) {
            const uint8_t *e = t + (size_t)i * 0x20;
            uint32_t id = be32(e);
            if (id == SCR_ENTRY_SFO) {
                sfo_off = be32(e + 0x10);
                sfo_sz = be32(e + 0x14);
            } else if (id == SCR_ENTRY_JSON) {
                js_off = be32(e + 0x10);
                js_sz = be32(e + 0x14);
            }
        }
        if (js_sz && base + js_off + js_sz <= size)
            read_version(read_at, ctx, base + js_off, js_sz, 1, out->version, sizeof out->version);
        if (!out->version[0] && sfo_sz && base + sfo_off + sfo_sz <= size)
            read_version(read_at, ctx, base + sfo_off, sfo_sz, 0, out->version, sizeof out->version);
    }
    free(t);
    return 0;
}

int scr_parse_id_list(const char *json, size_t n, const char *key, uint32_t *out, int max) {
    size_t klen = strlen(key);
    if (!json) return -1;
    for (size_t i = 0; i + klen + 2 <= n; i++) {
        if (json[i] != '"' || memcmp(json + i + 1, key, klen) != 0 || json[i + 1 + klen] != '"') continue;
        size_t j = i + 2 + klen;
        while (j < n && json[j] == ' ') j++;
        if (j >= n || json[j] != ':') return -1;
        j++;
        while (j < n && json[j] == ' ') j++;
        if (j >= n || json[j] != '[') return -1;
        j++;
        int count = 0, after_comma = 0;
        for (;;) {
            while (j < n && (json[j] == ' ' || json[j] == '\n' || json[j] == '\t' || json[j] == '\r')) j++;
            if (j < n && json[j] == ']' && !after_comma) return count;
            if (j >= n || json[j] < '0' || json[j] > '9') return -1;
            uint64_t v = 0;
            while (j < n && json[j] >= '0' && json[j] <= '9') {
                v = v * 10u + (uint64_t)(json[j] - '0');
                if (v > 0xFFFFFFFFu) return -1;
                j++;
            }
            if (count < max) out[count++] = (uint32_t)v;
            else return -1;
            while (j < n && (json[j] == ' ' || json[j] == '\n' || json[j] == '\t' || json[j] == '\r')) j++;
            if (j < n && json[j] == ',') {
                j++;
                after_comma = 1;
                continue;
            }
            if (j < n && json[j] == ']') return count;
            return -1;
        }
    }
    return -1;
}

static int starts(const char *s, const char *p) { return strncmp(s, p, strlen(p)) == 0; }

const char *scr_drive_of(const char *path) {
    static const char *const usb[] = { "usb0", "usb1", "usb2", "usb3", "usb4", "usb5", "usb6", "usb7" };
    if (!path) return "";
    if (starts(path, "/mnt/ext0/") || strcmp(path, "/mnt/ext0") == 0) return "ext0";
    if (starts(path, "/mnt/ext1/") || strcmp(path, "/mnt/ext1") == 0) return "ext1";
    if (starts(path, "/mnt/usb") && path[8] >= '0' && path[8] <= '7' && (path[9] == '/' || path[9] == '\0'))
        return usb[path[8] - '0'];
    if (starts(path, "/user/") || starts(path, "/data/") || strcmp(path, "/data") == 0 || strcmp(path, "/user") == 0)
        return "internal";
    if (starts(path, "/mnt/ps5upload/")) return "internal";
    return "";
}

int scr_path_has_dot_parts(const char *path) {
    const char *p = path;
    if (!p || *p != '/') return 1;
    while (*p) {
        const char *s = p + 1;
        const char *e = strchr(s, '/');
        size_t len = e ? (size_t)(e - s) : strlen(s);
        if (len == 0) return 1;
        if (len == 1 && s[0] == '.') return 1;
        if (len == 2 && s[0] == '.' && s[1] == '.') return 1;
        if (!e) break;
        p = e;
    }
    return 0;
}

/* `path` minus `prefix`, when it starts with it; else NULL. */
static const char *after(const char *path, const char *prefix) {
    size_t n = strlen(prefix);
    return strncmp(path, prefix, n) == 0 ? path + n : NULL;
}

/* The drive root of a path for C/D ("/data", "/user/data", "/mnt/ext0", "/mnt/usb3") and the rest
 * after its slash. 1 found. */
static int split_drive(const char *path, char *root, size_t cap, const char **rel) {
    static const char *const fixed[] = { "/user/data/", "/data/", "/mnt/ext0/", "/mnt/ext1/" };
    for (size_t i = 0; i < sizeof fixed / sizeof fixed[0]; i++) {
        const char *r = after(path, fixed[i]);
        if (r) {
            size_t n = strlen(fixed[i]) - 1;
            if (n >= cap) return 0;
            memcpy(root, fixed[i], n);
            root[n] = '\0';
            /* /user/data is /data */
            if (strcmp(root, "/user/data") == 0) memcpy(root, "/data", 6);
            *rel = r;
            return 1;
        }
    }
    if (starts(path, "/mnt/usb") && path[8] >= '0' && path[8] <= '7' && path[9] == '/') {
        if (cap < 10) return 0;
        memcpy(root, path, 9);
        root[9] = '\0';
        *rel = path + 10;
        return 1;
    }
    return 0;
}

int scr_walk_skip(const char *drive_root, const char *rel) {
    static const char *const data_skip[] = { "shadowmount", "shadowmountplus", "pldmgr", "etaHEN", "nanodns",
                                             "byepervisor", "kstuff" };
    if (!rel || !rel[0]) return 0;
    /* games and images live in homebrew/: its files are looked at, its folders never entered */
    if (starts(rel, "homebrew/")) return 1;
    /* ps5upload's own folders: only downloads/ (links fetched to the console) is searched */
    if (starts(rel, "ps5upload/") && !starts(rel, "ps5upload/downloads")) return 1;
    if (strcmp(drive_root, "/data") == 0) {
        for (size_t i = 0; i < sizeof data_skip / sizeof data_skip[0]; i++) {
            size_t n = strlen(data_skip[i]);
            if (strncmp(rel, data_skip[i], n) == 0 && (rel[n] == '\0' || rel[n] == '/')) return 1;
        }
        return 0;
    }
    if (starts(drive_root, "/mnt/ext")) {
        if (strcmp(rel, "user") == 0 || starts(rel, "user/") || strcmp(rel, "system_data") == 0 ||
            starts(rel, "system_data/"))
            return 1;
    }
    return 0;
}

static int ends(const char *s, const char *suf) {
    size_t a = strlen(s), b = strlen(suf);
    return a >= b && strcmp(s + a - b, suf) == 0;
}

static int one_component(const char *s) { return s && s[0] && strchr(s, '/') == NULL; }

int scr_path_allowed(char cat, const char *path, const char *tid) {
    static const char *const user_roots[] = { "/user/", "/mnt/ext0/user/", "/mnt/ext1/user/" };
    const char *r;
    if (!path || strlen(path) >= 1024 || scr_path_has_dot_parts(path)) return 0;
    switch (cat) {
    case 'A':
    case 'B':
    case 'F':
        if (!scr_is_title_id(tid) || scr_is_system_title(tid)) return 0;
        for (size_t i = 0; i < sizeof user_roots / sizeof user_roots[0]; i++) {
            char pre[48];
            snprintf(pre, sizeof pre, "%s%s/", user_roots[i], cat == 'F' ? "appmeta" : "patch");
            if ((r = after(path, pre)) && strcmp(r, tid) == 0) return 1;
        }
        if (cat == 'F') {
            static const char *const playgo[] = { "/user/playgo/app/", "/user/playgo/patch/", "/user/playgo/addcont/" };
            for (size_t i = 0; i < sizeof playgo / sizeof playgo[0]; i++) {
                char t[10];
                if ((r = after(path, playgo[i])) && one_component(r) && scr_tid_of_content_id(r, t) &&
                    strcmp(t, tid) == 0)
                    return 1;
            }
        }
        return 0;
    case 'C': {
        char root[16];
        const char *rel;
        if (!(ends(path, ".pkg") || ends(path, ".pkg.part") || ends(path, ".PKG"))) return 0;
        if (!split_drive(path, root, sizeof root, &rel) || !rel[0]) return 0;
        /* every folder on the way must be one the walk enters */
        char dir[1024];
        size_t n = strlen(rel);
        if (n >= sizeof dir) return 0;
        memcpy(dir, rel, n + 1);
        for (char *s = strchr(dir, '/'); s; s = strchr(s + 1, '/')) {
            *s = '\0';
            int skip = scr_walk_skip(root, dir);
            *s = '/';
            if (skip) return 0;
        }
        return 1;
    }
    case 'D': {
        char root[16];
        const char *rel;
        if ((r = after(path, "/mnt/ps5upload/.unpack/"))) return one_component(r);
        if (!split_drive(path, root, sizeof root, &rel)) return 0;
        if ((r = after(rel, "ps5upload/pkg_temp/")) || (r = after(rel, "ps5upload/tests/"))) return one_component(r);
        return 0;
    }
    case 'E':
        if (!scr_is_title_id(tid) || scr_is_system_title(tid)) return 0;
        if (!(r = after(path, "/user/download/"))) return 0;
        return strncmp(r, tid, 9) == 0 && strcmp(r + 9, "/download0.dat") == 0;
    default:
        return 0;
    }
}

/* ---- Kraken Mount's scan roots ---- */

/* `path` with /user/data written as /data and trailing slashes dropped, into `out`. 0 when it is not
 * absolute or too long. */
static int kraken_norm(const char *path, char *out, size_t cap) {
    char tmp[1024];
    const char *p = path;
    if (!p || p[0] != '/') return 0;
    if (strncmp(p, "/user/data", 10) == 0 && (p[10] == '/' || p[10] == '\0')) {
        if ((size_t)snprintf(tmp, sizeof tmp, "/data%s", p + 10) >= sizeof tmp) return 0;
        p = tmp;
    }
    size_t n = strlen(p);
    while (n > 1 && p[n - 1] == '/') n--;
    if (n >= cap) return 0;
    memmove(out, p, n);
    out[n] = '\0';
    return 1;
}

static int trim_char(char c) { return c == ' ' || c == '\t' || c == '\r' || c == '[' || c == ']' || c == '"' || c == '\''; }

int scr_kraken_scanpaths(const char *toml, size_t n, char out[][SCR_KRAKEN_ROOT_LEN], int max) {
    int count = 0;
    size_t i = 0;
    if (!toml) return 0;
    while (i < n && count < max) {
        char line[1024];
        size_t e = i;
        while (e < n && toml[e] != '\n') e++;
        size_t ll = e - i < sizeof line - 1 ? e - i : sizeof line - 1;
        memcpy(line, toml + i, ll);
        line[ll] = '\0';
        i = e + 1;
        char *s = line;
        while (*s == ' ' || *s == '\t') s++;
        if (!*s || *s == '#' || *s == '[') continue;
        char *eq = strchr(s, '=');
        if (!eq) continue;
        char *ke = eq;
        while (ke > s && (ke[-1] == ' ' || ke[-1] == '\t')) ke--;
        if ((size_t)(ke - s) != 8 || strncmp(s, "scanpath", 8) != 0) continue;
        char *hash = strstr(eq + 1, " #");
        if (hash) *hash = '\0';
        for (char *part = eq + 1; part && count < max;) {
            char *comma = strchr(part, ',');
            if (comma) *comma = '\0';
            char *a = part, *b = part + strlen(part);
            while (a < b && trim_char(*a)) a++;
            while (b > a && trim_char(b[-1])) b--;
            *b = '\0';
            char norm[SCR_KRAKEN_ROOT_LEN];
            if (kraken_norm(a, norm, sizeof norm)) {
                int dup = 0;
                for (int q = 0; q < count; q++)
                    if (strcmp(out[q], norm) == 0) dup = 1;
                if (!dup) memcpy(out[count++], norm, strlen(norm) + 1);
            }
            part = comma ? comma + 1 : NULL;
        }
    }
    return count;
}

int scr_kraken_default_root(const char *dir) {
    char d[SCR_KRAKEN_ROOT_LEN];
    const char *r;
    if (!kraken_norm(dir, d, sizeof d)) return 0;
    if (strcmp(d, "/data/homebrew") == 0 || strcmp(d, "/data/etaHEN/games") == 0) return 1;
    if (strncmp(d, "/mnt/ext", 8) == 0 && (d[8] == '0' || d[8] == '1')) r = d + 9;
    else if (strncmp(d, "/mnt/usb", 8) == 0 && d[8] >= '0' && d[8] <= '7') r = d + 9;
    else return 0;
    return r[0] == '\0' || strcmp(r, "/homebrew") == 0 || strcmp(r, "/etaHEN/games") == 0;
}

int scr_kraken_keeps(const char *path, const char (*roots)[SCR_KRAKEN_ROOT_LEN], int nroots) {
    char p[1024];
    if (!kraken_norm(path, p, sizeof p)) return 0;
    char *slash = strrchr(p, '/');
    if (!slash || slash == p) return 0; /* nothing sits in "/" that it scans */
    *slash = '\0';
    if (nroots <= 0) return scr_kraken_default_root(p);
    for (int i = 0; i < nroots; i++) {
        char r[SCR_KRAKEN_ROOT_LEN];
        if (kraken_norm(roots[i], r, sizeof r) && strcmp(r, p) == 0) return 1;
    }
    return 0;
}

int scr_kraken_mountable(const char *path, int is_dir) {
    if (!path) return 0;
    if (is_dir) return 1;
    size_t n = strlen(path);
    return n > 4 && path[n - 4] == '.' && (path[n - 3] | 0x20) == 'p' && (path[n - 2] | 0x20) == 'k' &&
           (path[n - 1] | 0x20) == 'g';
}
