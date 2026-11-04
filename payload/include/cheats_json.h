#ifndef PS5UPLOAD_CHEATS_JSON_H
#define PS5UPLOAD_CHEATS_JSON_H

/* The string fields of a JSON cheat file (cheats.c).
 *
 * Community files are not always strict JSON and their key order is not fixed, so this stays a lenient
 * text scan and never rejects a file. Two lookups: cj_str finds the first "key":"value" at any depth,
 * which is what a mod's own small object wants; cj_top_str finds a key of the file itself, which is
 * what the game's name wants. A file written {"id":"..","mods":[{"name":"God mode"}],"name":"Game"} has
 * the game's name after a cheat's, and the first match named the game "God mode".
 *
 * Header-only and free of the ptrace / Sony code in cheats.c, so it builds and tests on the host. */
#include <stddef.h>
#include <string.h>

static inline int cj_hex(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Four hex digits at p, or -1. A NUL is not a digit, so this never reads past the end of the text. */
static inline long cj_hex4(const char *p) {
    long v = 0;
    for (int i = 0; i < 4; i++) {
        int h = cj_hex(p[i]);
        if (h < 0) return -1;
        v = v * 16 + h;
    }
    return v;
}

/* One code point as UTF-8 at out[i]: the new i, or i unchanged when it does not fit whole. */
static inline size_t cj_put_cp(char *out, size_t i, size_t cap, unsigned long cp) {
    if (cp < 0x80) {
        if (i + 1 >= cap) return i;
        out[i++] = (char)cp;
    } else if (cp < 0x800) {
        if (i + 2 >= cap) return i;
        out[i++] = (char)(0xc0 | (cp >> 6));
        out[i++] = (char)(0x80 | (cp & 0x3f));
    } else if (cp < 0x10000) {
        if (i + 3 >= cap) return i;
        out[i++] = (char)(0xe0 | (cp >> 12));
        out[i++] = (char)(0x80 | ((cp >> 6) & 0x3f));
        out[i++] = (char)(0x80 | (cp & 0x3f));
    } else {
        if (i + 4 >= cap) return i;
        out[i++] = (char)(0xf0 | (cp >> 18));
        out[i++] = (char)(0x80 | ((cp >> 12) & 0x3f));
        out[i++] = (char)(0x80 | ((cp >> 6) & 0x3f));
        out[i++] = (char)(0x80 | (cp & 0x3f));
    }
    return i;
}

/* The string whose first byte (after the opening quote) is p, unescaped into out: cut at cap, never
 * mid-character when the cut falls on an escape. \uXXXX becomes UTF-8, surrogate pairs joined; a lone
 * surrogate or \u0000 becomes U+FFFD. Any other unknown escape keeps its letter. */
static inline void cj_decode(const char *p, char *out, size_t cap) {
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < cap) {
        if (*p != '\\' || !p[1]) {
            out[i++] = *p++;
            continue;
        }
        char e = p[1];
        if (e == 'u') {
            long v = cj_hex4(p + 2);
            if (v >= 0) {
                size_t at;
                p += 6;
                if (v >= 0xd800 && v <= 0xdbff && p[0] == '\\' && p[1] == 'u') {
                    long lo = cj_hex4(p + 2);
                    if (lo >= 0xdc00 && lo <= 0xdfff) {
                        v = 0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00);
                        p += 6;
                    }
                }
                if (v == 0 || (v >= 0xd800 && v <= 0xdfff)) v = 0xfffd;
                at = cj_put_cp(out, i, cap, (unsigned long)v);
                if (at == i) break; /* no room for it whole */
                i = at;
                continue;
            }
        }
        switch (e) {
            case 'n': out[i++] = '\n'; break;
            case 't': out[i++] = '\t'; break;
            case 'r': out[i++] = '\r'; break;
            case 'b': out[i++] = '\b'; break;
            case 'f': out[i++] = '\f'; break;
            default: out[i++] = e; break; /* \" \\ \/ and anything unknown */
        }
        p += 2;
    }
    out[i] = '\0';
}

/* The first `"key"` followed by a colon and a string, before `limit` (NULL: anywhere): 0 with the value
 * in out, -1 not found or not a string. */
static inline int cj_str_before(const char *json, const char *limit, const char *key, char *out, size_t cap) {
    if (!json || !key || !out || cap == 0) return -1;
    out[0] = '\0';
    size_t klen = strlen(key);
    const char *p = json;
    while ((p = strstr(p, key)) && (!limit || p < limit)) {
        const char *q = p + klen;
        if (p > json && p[-1] == '"' && q[0] == '"' && (q[1] == ':' || q[1] == ' ' || q[1] == '\t')) {
            p = q + 1;
            while (*p && *p != ':') p++;
            if (*p != ':') return -1;
            p++;
            while (*p == ' ' || *p == '\t') p++;
            if (*p != '"') return -1;
            cj_decode(p + 1, out, cap);
            return 0;
        }
        p += klen;
    }
    return -1;
}

/* Any depth. */
static inline int cj_str(const char *json, const char *key, char *out, size_t cap) {
    return cj_str_before(json, NULL, key, out, cap);
}

/* p at the opening quote: the byte after the closing one, or NULL when the text ends first. */
static inline const char *cj_skip_string(const char *p) {
    for (p++; *p; p++) {
        if (*p == '\\') {
            if (!p[1]) return NULL;
            p++;
        } else if (*p == '"') {
            return p + 1;
        }
    }
    return NULL;
}

/* A key of the file's outermost object: the first byte of its value, or NULL. Only brackets outside
 * strings count, so a BOM, a trailing comma or any such slip does not lose it. */
static inline const char *cj_top_value(const char *json, const char *key) {
    size_t klen = strlen(key);
    int depth = 0;
    for (const char *p = json; *p;) {
        if (*p == '"') {
            const char *end = cj_skip_string(p);
            if (!end) return NULL;
            if (depth == 1 && (size_t)(end - p - 2) == klen && memcmp(p + 1, key, klen) == 0) {
                const char *q = end;
                while (*q == ' ' || *q == '\t' || *q == '\r' || *q == '\n') q++;
                if (*q == ':') {
                    q++;
                    while (*q == ' ' || *q == '\t' || *q == '\r' || *q == '\n') q++;
                    return q;
                }
            }
            p = end;
            continue;
        }
        if (*p == '{' || *p == '[') {
            depth++;
        } else if (*p == '}' || *p == ']') {
            if (--depth <= 0) return NULL; /* the outermost object ended */
        }
        p++;
    }
    return NULL;
}

/* A field of the file itself: its outermost object's own key, wherever it sits. A file that wraps its
 * fields one level down ({"game":{"name":".."},"mods":[..]}) still has them before its mods, so that
 * is looked at next; a cheat's own name, inside the mods, never is. 0 with the value, or -1. */
static inline int cj_top_str(const char *json, const char *key, char *out, size_t cap) {
    if (!json || !key || !out || cap == 0) return -1;
    out[0] = '\0';
    const char *v = cj_top_value(json, key);
    if (v && *v == '"') {
        cj_decode(v + 1, out, cap);
        return 0;
    }
    return cj_str_before(json, strstr(json, "\"mods\""), key, out, cap);
}

#endif /* PS5UPLOAD_CHEATS_JSON_H */
