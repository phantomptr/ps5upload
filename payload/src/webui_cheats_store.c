/* See include/webui_cheats_store.h. */
#include "webui_cheats_store.h"

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

#include "appinfo_version.h"
#include "webui_codec.h"

int webui_cheat_format_ok(const char *fmt) {
    return fmt && (strcmp(fmt, "json") == 0 || strcmp(fmt, "shn") == 0 || strcmp(fmt, "mc4") == 0);
}

static int name_byte(char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '_' ||
           c == '-';
}

int webui_cheat_name_ok(const char *fmt, const char *name) {
    size_t n, fl, i;
    if (!webui_cheat_format_ok(fmt) || !name) return 0;
    n = strlen(name);
    fl = strlen(fmt);
    if (n == 0 || n > WEBUI_CHEAT_NAME_MAX || name[0] == '.' || strstr(name, "..")) return 0;
    for (i = 0; i < n; i++)
        if (!name_byte(name[i])) return 0;
    /* a stem, then ".<fmt>": the engine picks files by that extension (cheats.c find_cheat_files) */
    if (n < fl + 2 || name[n - fl - 1] != '.' || strcasecmp(name + n - fl, fmt) != 0) return 0;
    return 1;
}

/* The value of `key` in the query into `out`: 1 found once, 0 absent, -1 twice or too long. */
static int query_value(const char *q, const char *key, char *out, size_t cap) {
    size_t kl = strlen(key);
    int found = 0;
    const char *p = q;
    while (p && *p) {
        const char *end = strchr(p, '&');
        size_t len = end ? (size_t)(end - p) : strlen(p);
        if (len > kl && strncmp(p, key, kl) == 0 && p[kl] == '=') {
            size_t vl = len - kl - 1;
            if (found || vl >= cap) return -1;
            memcpy(out, p + kl + 1, vl);
            out[vl] = '\0';
            found = 1;
        } else if (len == kl && strncmp(p, key, kl) == 0) {
            return -1; /* "key" without a value */
        }
        p = end ? end + 1 : NULL;
    }
    return found;
}

const char *webui_cheat_store_query(const char *query, char fmt[WEBUI_CHEAT_FMT_CAP],
                                    char name[WEBUI_CHEAT_NAME_MAX + 1]) {
    fmt[0] = '\0';
    name[0] = '\0';
    if (!query) return "bad_format";
    if (query_value(query, "format", fmt, WEBUI_CHEAT_FMT_CAP) != 1 || !webui_cheat_format_ok(fmt)) return "bad_format";
    if (query_value(query, "name", name, WEBUI_CHEAT_NAME_MAX + 1) != 1 || !webui_cheat_name_ok(fmt, name))
        return "bad_name";
    return NULL;
}

/* `path` is a real folder after this (made when missing): 0, else -1. */
static int ensure_dir(const char *path) {
    struct stat st;
    if (mkdir(path, 0777) != 0 && errno != EEXIST) return -1;
    if (lstat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return -1;
    (void)chmod(path, 0777); /* the umask must not take the bits the helper's folders always have */
    return 0;
}

int webui_cheat_target(const char *root, const char *fmt, const char *name, char *out, size_t cap) {
    char dir[512];
    int k;
    if (!root || !webui_cheat_name_ok(fmt, name)) return -1;
    k = snprintf(dir, sizeof dir, "%s/%s", root, fmt);
    if (k <= 0 || (size_t)k >= sizeof dir) return -1;
    k = snprintf(out, cap, "%s/%s", dir, name);
    if (k <= 0 || (size_t)k >= cap) return -1;
    if (ensure_dir(root) != 0 || ensure_dir(dir) != 0) return -1;
    return 0;
}

int webui_cheat_file_title(const char *name, char out[10]) {
    int i;
    out[0] = '\0';
    if (!name) return 0;
    for (i = 0; i < 9; i++) {
        char c = name[i];
        int ok = i < 4 ? ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) : (c >= '0' && c <= '9');
        if (!ok) return 0;
    }
    if (name[9] != '.' && name[9] != '_') return 0;
    for (i = 0; i < 9; i++) out[i] = (char)(name[i] >= 'a' && name[i] <= 'z' ? name[i] - 'a' + 'A' : name[i]);
    out[9] = '\0';
    return 1;
}

int webui_cheat_state_path(const char *root, const char *title, char *out, size_t cap) {
    char check[16];
    int k;
    if (!root || !title || strlen(title) != 9) return -1;
    memcpy(check, title, 9);
    check[9] = '.';
    check[10] = '\0';
    {
        char upper[10];
        if (!webui_cheat_file_title(check, upper) || strcmp(upper, title) != 0) return -1;
    }
    k = snprintf(out, cap, "%s/state/%s.json", root, title);
    return k > 0 && (size_t)k < cap ? 0 : -1;
}

/* The end of the JSON value at s[0] (an object): the index past its '}', strings skipped; 0 if cut. */
static size_t object_end(const char *s, size_t n) {
    size_t i;
    int depth = 0, in_str = 0;
    for (i = 0; i < n; i++) {
        char c = s[i];
        if (in_str) {
            if (c == '\\') i++;
            else if (c == '"') in_str = 0;
        } else if (c == '"') {
            in_str = 1;
        } else if (c == '{' || c == '[') {
            depth++;
        } else if (c == '}' || c == ']') {
            if (--depth == 0) return i + 1;
        }
    }
    return 0;
}

/* `v` trimmed of spaces in place; 1 when it is then digits and dots with a digit first. */
static int version_ok(char *v) {
    size_t i, n, a = 0;
    while (v[a] == ' ') a++;
    if (a) memmove(v, v + a, strlen(v + a) + 1);
    n = strlen(v);
    while (n && v[n - 1] == ' ') v[--n] = '\0';
    if (n == 0 || v[0] < '0' || v[0] > '9') return 0;
    for (i = 0; i < n; i++)
        if (v[i] != '.' && (v[i] < '0' || v[i] > '9')) return 0;
    return 1;
}

int webui_cheat_appinfo_version(const char *json, size_t n, char *out, size_t cap) {
    const char *arr;
    size_t alen, i = 1;
    char key[32], val[64], version[64] = "", app_ver[64] = "";
    const char *pick;
    if (cap) out[0] = '\0';
    if (!json || cap < 2 || webui_json_array(json, n, "rows", &arr, &alen) != 1) return 0;
    while (i < alen) {
        size_t len;
        char c = arr[i];
        if (c != '{') {
            i++;
            continue;
        }
        len = object_end(arr + i, alen - i);
        if (!len) break;
        if (webui_json_str(arr + i, len, "key", key, sizeof key) == 1 &&
            webui_json_str(arr + i, len, "val", val, sizeof val) == 1 && version_ok(val) && strlen(val) < cap) {
            if (strcmp(key, "CONTENT_VERSION") == 0) {
                memcpy(out, val, strlen(val) + 1);
                return 1;
            }
            if (strcmp(key, "VERSION") == 0) memcpy(version, val, strlen(val) + 1);
            if (strcmp(key, "APP_VER") == 0) memcpy(app_ver, val, strlen(val) + 1);
        }
        i += len;
    }
    /* A PS4 update raises VERSION and leaves APP_VER at the base game's "01.00" (#934). */
    pick = appinfo_version_pick(NULL, version, app_ver);
    if (pick[0]) {
        memcpy(out, pick, strlen(pick) + 1);
        return 1;
    }
    return 0;
}
