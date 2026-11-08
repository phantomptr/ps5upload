/* See include/pkg_info.h. Mirrors engine/crates/ps5upload-pkg metadata_from_reader + walk_entries:
 * a CNT header (or a PS5 \x7FFIH envelope pointing at one), its entry table, PARAM.SFO (0x1000),
 * param.json (0x2000) and icon0.png (0x1200). */
#include "pkg_info.h"

#include <stdlib.h>
#include <string.h>

#define PKG_MAGIC_CNT 0x7F434E54u
#define PKG_MAGIC_FIH 0x7F464948u
#define ENTRY_PARAM_SFO 0x1000u
#define ENTRY_PARAM_JSON 0x2000u
#define ENTRY_ICON0_PNG 0x1200u
#define ENTRY_FLAG_ENCRYPTED 0x80000000u
#define HEAD_LEN 0xA0u

static uint32_t be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}
static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }
static uint64_t le64(const uint8_t *p) { return (uint64_t)le32(p) | ((uint64_t)le32(p + 4) << 32); }

/* [off, off+len) inside a file of `size` bytes, without wrapping. */
static int in_file(uint64_t off, uint64_t len, uint64_t size) { return off <= size && len <= size - off; }

/* Copies a NUL-padded byte field as text: stops at the first NUL, never past `len`, trims spaces. */
static void copy_text(char *out, size_t cap, const uint8_t *s, size_t len) {
    size_t n = 0;
    while (n < len && s[n]) n++;
    while (n && (s[n - 1] == ' ' || s[n - 1] == '\t' || s[n - 1] == '\r' || s[n - 1] == '\n')) n--;
    if (n >= cap) n = cap - 1;
    memcpy(out, s, n);
    out[n] = '\0';
}

void pkg_info_fw_text(uint32_t v, char *out, size_t cap) {
    static const char hex[] = "0123456789abcdef";
    unsigned major = (v >> 24) & 0xFFu, minor = (v >> 16) & 0xFFu;
    char m[4];
    size_t k = 0;
    if (!cap) return;
    out[0] = '\0';
    if (v == 0 || cap < 7) return;
    /* the bytes read as hex digits: 0x05 0x05 -> "5.05", 0x10 0x00 -> "10.00" */
    if (major >> 4) m[k++] = hex[major >> 4];
    m[k++] = hex[major & 0xF];
    memcpy(out, m, k);
    out[k++] = '.';
    out[k++] = hex[minor >> 4];
    out[k++] = hex[minor & 0xF];
    out[k] = '\0';
}

/* ---- PARAM.SFO ---- */

/* PUBTOOLINFO "c_date=20200101,sdk_ver=05050001,st_type=digital50": sdk_ver's BCD word as "5.05". */
static void sfo_sdk(const uint8_t *b, size_t len, pkg_info_t *o) {
    static const char key[] = "sdk_ver=";
    size_t i, k;
    for (i = 0; i + sizeof key - 1 + 8 <= len; i++) {
        uint32_t v = 0;
        if ((i > 0 && b[i - 1] != ',') || memcmp(b + i, key, sizeof key - 1) != 0) continue;
        for (k = 0; k < 8; k++) {
            uint8_t c = b[i + sizeof key - 1 + k];
            if (c < '0' || c > '9') return;
            v = (v << 4) | (uint32_t)(c - '0');
        }
        pkg_info_fw_text(v, o->sdk, sizeof o->sdk);
        return;
    }
}

static void parse_sfo(const uint8_t *b, size_t n, pkg_info_t *o, uint32_t *sys_ver) {
    uint32_t keys, data, count, i;
    if (n < 20 || memcmp(b, "\0PSF", 4) != 0) return;
    keys = le32(b + 8);
    data = le32(b + 12);
    count = le32(b + 16);
    if (keys > n || data > n || count > 256) return;
    for (i = 0; i < count; i++) {
        uint64_t e = 20u + (uint64_t)i * 16u, kabs, dabs;
        uint16_t fmt;
        uint32_t len;
        const char *key;
        size_t kmax, klen;
        if (e + 16 > n) break;
        fmt = le16(b + e + 2);
        len = le32(b + e + 4);
        kabs = (uint64_t)keys + le16(b + e);
        dabs = (uint64_t)data + le32(b + e + 12);
        if (kabs >= n || !in_file(dabs, len, n)) continue;
        key = (const char *)b + kabs;
        kmax = n - (size_t)kabs;
        klen = strnlen(key, kmax);
        if (klen == kmax) continue; /* unterminated key */
        if (fmt == 0x0404) {
            if (len >= 4 && strcmp(key, "SYSTEM_VER") == 0) *sys_ver = le32(b + dabs);
            continue;
        }
        if (fmt != 0x0004 && fmt != 0x0204) continue;
        if (strcmp(key, "TITLE") == 0) copy_text(o->title, sizeof o->title, b + dabs, len);
        else if (strcmp(key, "TITLE_ID") == 0) copy_text(o->title_id, sizeof o->title_id, b + dabs, len);
        else if (strcmp(key, "CATEGORY") == 0) copy_text(o->category, sizeof o->category, b + dabs, len);
        else if (strcmp(key, "APP_VER") == 0) copy_text(o->app_ver, sizeof o->app_ver, b + dabs, len);
        else if (strcmp(key, "CONTENT_ID") == 0 && !o->content_id[0])
            copy_text(o->content_id, sizeof o->content_id, b + dabs, len);
        else if (strcmp(key, "PUBTOOLINFO") == 0) sfo_sdk(b + dabs, len, o);
    }
}

/* ---- param.json: a bounded scan for the few string keys it carries (no general JSON parser) ---- */

static size_t skip_ws(const char *s, size_t n, size_t i) {
    while (i < n && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n')) i++;
    return i;
}

/* Past the string that opens at s[i] == '"' (the index after its closing quote), or n. */
static size_t skip_string(const char *s, size_t n, size_t i) {
    for (i++; i < n; i++) {
        if (s[i] == '\\') i++;
        else if (s[i] == '"') return i + 1;
    }
    return n;
}

/* Past the value at s[i]: a string, or an object/array with its nesting (strings inside skipped). */
static size_t skip_value(const char *s, size_t n, size_t i) {
    int depth = 0;
    i = skip_ws(s, n, i);
    if (i < n && s[i] == '"') return skip_string(s, n, i);
    for (; i < n; i++) {
        char c = s[i];
        if (c == '"') {
            i = skip_string(s, n, i) - 1;
        } else if (c == '{' || c == '[') {
            depth++;
        } else if (c == '}' || c == ']') {
            if (depth == 0) return i;
            if (--depth == 0) return i + 1;
        } else if (depth == 0 && c == ',') {
            return i;
        }
    }
    return n;
}

/* The value after the first `"key":` in s[from, to), as its index, or `to`. A key is only matched
 * where a string starts, never inside another string. */
static size_t find_key(const char *s, size_t to, size_t from, const char *key) {
    size_t kl = strlen(key), i = from;
    while (i < to) {
        if (s[i] != '"') {
            i++;
            continue;
        }
        if (i + 1 + kl < to && memcmp(s + i + 1, key, kl) == 0 && s[i + 1 + kl] == '"') {
            size_t j = skip_ws(s, to, i + 2 + kl);
            if (j < to && s[j] == ':') return skip_ws(s, to, j + 1);
        }
        i = skip_string(s, to, i);
    }
    return to;
}

static void put_utf8(char *out, size_t cap, size_t *k, uint32_t cp) {
    uint8_t b[4];
    size_t m, j;
    if (cp < 0x80) {
        b[0] = (uint8_t)cp;
        m = 1;
    } else if (cp < 0x800) {
        b[0] = (uint8_t)(0xC0 | (cp >> 6));
        b[1] = (uint8_t)(0x80 | (cp & 0x3F));
        m = 2;
    } else if (cp < 0x10000) {
        b[0] = (uint8_t)(0xE0 | (cp >> 12));
        b[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
        b[2] = (uint8_t)(0x80 | (cp & 0x3F));
        m = 3;
    } else {
        b[0] = (uint8_t)(0xF0 | (cp >> 18));
        b[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F));
        b[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
        b[3] = (uint8_t)(0x80 | (cp & 0x3F));
        m = 4;
    }
    if (*k + m >= cap) return;
    for (j = 0; j < m; j++) out[(*k)++] = (char)b[j];
}

static int hex4(const char *s, size_t n, size_t i, uint32_t *v) {
    size_t j;
    *v = 0;
    if (i + 4 > n) return -1;
    for (j = 0; j < 4; j++) {
        char c = s[i + j];
        *v <<= 4;
        if (c >= '0' && c <= '9') *v |= (uint32_t)(c - '0');
        else if (c >= 'a' && c <= 'f') *v |= (uint32_t)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F') *v |= (uint32_t)(c - 'A' + 10);
        else return -1;
    }
    return 0;
}

/* The string value at s[i] (unescaped, cut to cap) into out: 1, or 0 when it is not a string. */
static int read_string(const char *s, size_t n, size_t i, char *out, size_t cap) {
    size_t k = 0;
    if (i >= n || s[i] != '"' || !cap) return 0;
    for (i++; i < n && s[i] != '"'; i++) {
        char c = s[i];
        if (c == '\\' && i + 1 < n) {
            char e = s[++i];
            uint32_t cp;
            switch (e) {
            case 'n': c = '\n'; break;
            case 't': c = '\t'; break;
            case 'r': c = '\r'; break;
            case 'b': c = '\b'; break;
            case 'f': c = '\f'; break;
            case 'u':
                if (hex4(s, n, i + 1, &cp) != 0) return 0;
                i += 4;
                if (cp >= 0xD800 && cp < 0xDC00 && i + 6 < n && s[i + 1] == '\\' && s[i + 2] == 'u') {
                    uint32_t lo;
                    if (hex4(s, n, i + 3, &lo) == 0 && lo >= 0xDC00 && lo < 0xE000) {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        i += 6;
                    }
                }
                put_utf8(out, cap, &k, cp);
                continue;
            default: c = e; break; /* \" \\ \/ */
            }
        }
        if (k + 1 < cap) out[k++] = c;
    }
    out[k] = '\0';
    return 1;
}

static void json_str(const char *s, size_t n, size_t from, size_t to, const char *key, char *out, size_t cap) {
    char tmp[256];
    size_t at = find_key(s, to, from, key);
    if (at < to && read_string(s, to, at, tmp, sizeof tmp) && tmp[0]) {
        size_t l = strlen(tmp);
        if (l >= cap) l = cap - 1;
        memcpy(out, tmp, l);
        out[l] = '\0';
    }
    (void)n;
}

/* "0x0750000000000000" (or a bare hex string) -> its top 32 bits. 0 when unreadable. */
static uint32_t hex_fw(const char *t) {
    uint64_t v = 0;
    int digits = 0;
    if (t[0] == '0' && (t[1] == 'x' || t[1] == 'X')) t += 2;
    for (; *t && digits < 16; t++, digits++) {
        char c = *t;
        v <<= 4;
        if (c >= '0' && c <= '9') v |= (uint64_t)(c - '0');
        else if (c >= 'a' && c <= 'f') v |= (uint64_t)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F') v |= (uint64_t)(c - 'A' + 10);
        else return 0;
    }
    if (digits == 0) return 0;
    v <<= 4 * (16 - digits); /* a short string is the leading digits */
    return (uint32_t)(v >> 32);
}

static void parse_param_json(const char *s, size_t n, pkg_info_t *o, uint32_t *sys_ver) {
    char lang[32] = "", fw[40] = "";
    size_t loc, end;
    while (n && (s[n - 1] == '\0')) n--; /* NUL padding */
    json_str(s, n, 0, n, "contentId", o->content_id, sizeof o->content_id);
    json_str(s, n, 0, n, "titleId", o->title_id, sizeof o->title_id);
    json_str(s, n, 0, n, "contentVersion", o->app_ver, sizeof o->app_ver);
    if (!o->app_ver[0]) json_str(s, n, 0, n, "masterVersion", o->app_ver, sizeof o->app_ver);
    json_str(s, n, 0, n, "requiredSystemSoftwareVersion", fw, sizeof fw);
    if (fw[0]) {
        uint32_t v = hex_fw(fw);
        if (v) *sys_ver = v;
    }
    fw[0] = '\0';
    json_str(s, n, 0, n, "sdkVersion", fw, sizeof fw);
    if (fw[0]) pkg_info_fw_text(hex_fw(fw), o->sdk, sizeof o->sdk);
    loc = find_key(s, n, 0, "localizedParameters");
    if (loc >= n || s[loc] != '{') return;
    end = skip_value(s, n, loc);
    if (end > n) end = n;
    /* the default language's titleName, else the first one listed */
    json_str(s, n, loc, end, "defaultLanguage", lang, sizeof lang);
    if (lang[0]) {
        size_t at = find_key(s, end, loc + 1, lang);
        if (at < end && s[at] == '{') {
            size_t lend = skip_value(s, end, at);
            char t[256] = "";
            json_str(s, n, at, lend, "titleName", t, sizeof t);
            if (t[0]) {
                memcpy(o->title, t, strlen(t) + 1);
                return;
            }
        }
    }
    json_str(s, n, loc, end, "titleName", o->title, sizeof o->title);
}

/* ---- the package ---- */

static void derive_platform(pkg_info_t *o) {
    const char *tok = o->title_id;
    char mid[16] = "";
    if (o->fih) {
        memcpy(o->platform, "ps5", 4);
        return;
    }
    if (!tok[0]) {
        const char *d = strchr(o->content_id, '-');
        if (d) {
            size_t k = 0;
            for (d++; *d && *d != '-' && *d != '_' && k + 1 < sizeof mid; d++) mid[k++] = *d;
            mid[k] = '\0';
        }
        tok = mid;
    }
    if (strncmp(tok, "CUSA", 4) == 0) memcpy(o->platform, "ps4", 4);
    else if (strncmp(tok, "PPSA", 4) == 0) memcpy(o->platform, "ps5", 4);
}

/* "EP4293-CUSA32097_00-..." -> "CUSA32097" when the package gave no title id. */
static void title_from_content(pkg_info_t *o) {
    const char *d = strchr(o->content_id, '-');
    size_t k = 0;
    if (o->title_id[0] || !d) return;
    for (d++; *d && *d != '_' && *d != '-' && k < 9; d++) o->title_id[k++] = *d;
    o->title_id[k] = '\0';
    if (k != 9) o->title_id[0] = '\0';
}

static const char *read_meta(pkg_read_fn rd, void *ctx, uint64_t size, uint64_t off, uint32_t len, uint8_t **out) {
    *out = NULL;
    if (len == 0 || len > PKG_INFO_MAX_META || !in_file(off, len, size)) return "range";
    *out = malloc(len);
    if (!*out) return "pkg_no_memory";
    if (rd(ctx, off, *out, len) != 0) {
        free(*out);
        *out = NULL;
        return "read";
    }
    return NULL;
}

const char *pkg_info_read(pkg_read_fn rd, void *ctx, uint64_t size, pkg_info_t *o) {
    uint8_t outer[HEAD_LEN], head[HEAD_LEN];
    uint64_t base = 0, table_off;
    uint32_t magic, count, i, sys_ver = 0;
    uint32_t sfo_off = 0, sfo_len = 0, js_off = 0, js_len = 0, ic_off = 0, ic_len = 0, ic_flags = 0;
    int have_sfo = 0, have_js = 0, have_ic = 0;
    uint8_t *table = NULL, *meta = NULL;
    memset(o, 0, sizeof *o);
    if (size < 0x60) return "pkg_not_pkg";
    if (rd(ctx, 0, outer, size < HEAD_LEN ? (size_t)size : HEAD_LEN) != 0) return "pkg_truncated";
    magic = be32(outer);
    if (magic == PKG_MAGIC_FIH) {
        if (le16(outer + 6) != 3) return "pkg_bad_header";
        base = le64(outer + 0x58);
        o->fih = 1;
        o->fake = outer[5] == 0x00;
        if (!in_file(base, HEAD_LEN, size)) return "pkg_truncated";
        if (rd(ctx, base, head, HEAD_LEN) != 0) return "pkg_truncated";
    } else if (magic == PKG_MAGIC_CNT) {
        if (size < HEAD_LEN) return "pkg_truncated";
        memcpy(head, outer, HEAD_LEN);
    } else {
        return "pkg_not_pkg";
    }
    if (be32(head) != PKG_MAGIC_CNT) return "pkg_bad_header";
    copy_text(o->content_id, sizeof o->content_id, head + 0x40, 36);
    count = be32(head + 0x10);
    table_off = base + be32(head + 0x18);
    if (count > 0 && count <= PKG_INFO_MAX_ENTRIES && in_file(table_off, (uint64_t)count * 0x20u, size)) {
        table = malloc((size_t)count * 0x20u);
        if (!table) return "pkg_no_memory";
        if (rd(ctx, table_off, table, (size_t)count * 0x20u) == 0) {
            for (i = 0; i < count; i++) {
                const uint8_t *e = table + (size_t)i * 0x20u;
                uint32_t id = be32(e);
                if (id == ENTRY_PARAM_SFO && !have_sfo) {
                    sfo_off = be32(e + 0x10);
                    sfo_len = be32(e + 0x14);
                    have_sfo = 1;
                } else if (id == ENTRY_PARAM_JSON && !have_js) {
                    js_off = be32(e + 0x10);
                    js_len = be32(e + 0x14);
                    have_js = 1;
                } else if (id == ENTRY_ICON0_PNG && !have_ic) {
                    ic_flags = be32(e + 0x04);
                    ic_off = be32(e + 0x10);
                    ic_len = be32(e + 0x14);
                    have_ic = 1;
                }
            }
        }
        free(table);
    }
    if (have_sfo && read_meta(rd, ctx, size, base + sfo_off, sfo_len, &meta) == NULL) {
        parse_sfo(meta, sfo_len, o, &sys_ver);
        free(meta);
    }
    if (have_js && read_meta(rd, ctx, size, base + js_off, js_len, &meta) == NULL) {
        parse_param_json((const char *)meta, js_len, o, &sys_ver);
        free(meta);
    }
    if (have_ic && !(ic_flags & ENTRY_FLAG_ENCRYPTED) && ic_len >= 8 && ic_len <= PKG_INFO_MAX_ICON &&
        in_file(base + ic_off, ic_len, size)) {
        uint8_t sig[8];
        static const uint8_t png[8] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n' };
        if (rd(ctx, base + ic_off, sig, sizeof sig) == 0 && memcmp(sig, png, sizeof png) == 0) {
            o->icon_off = base + ic_off;
            o->icon_len = ic_len;
        }
    }
    if (o->fih && !o->category[0]) {
        /* param.json has no category: the header says patch, add-on (0x21, #724) or game. */
        uint32_t type = be32(head + 0x74), flags = be32(head + 0x78);
        const char *cat = (flags & 0x00100000u) || (flags & 0x40000000u) ? "gp"
                          : (type == 0x21u || type == 0x1Bu || type == 0x1Cu) ? "ac"
                                                                               : "gd";
        memcpy(o->category, cat, 3);
    }
    title_from_content(o);
    derive_platform(o);
    pkg_info_fw_text(sys_ver, o->fw, sizeof o->fw);
    return NULL;
}

void pkg_info_from_meta(const uint8_t *b, size_t n, int is_json, pkg_info_t *o) {
    uint32_t sys_ver = 0;
    memset(o, 0, sizeof *o);
    if (is_json) parse_param_json((const char *)b, n, o, &sys_ver);
    else parse_sfo(b, n, o, &sys_ver);
    title_from_content(o);
    derive_platform(o);
    pkg_info_fw_text(sys_ver, o->fw, sizeof o->fw);
}

const char *pkg_info_kind(const pkg_info_t *p) {
    char c[8];
    size_t i;
    for (i = 0; i + 1 < sizeof c && p->category[i]; i++)
        c[i] = (char)(p->category[i] >= 'A' && p->category[i] <= 'Z' ? p->category[i] + 32 : p->category[i]);
    c[i] = '\0';
    if (strcmp(c, "gp") == 0 || strcmp(c, "dp") == 0) return "patch";
    if (strcmp(c, "ac") == 0) return "dlc";
    if (strcmp(c, "gd") == 0) return "game";
    if (c[0] == 'g' && c[1] == 'd') return "app"; /* gde, gda, gdc, ...: apps and system software */
    return "other";
}
