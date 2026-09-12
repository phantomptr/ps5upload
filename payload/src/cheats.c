/*
 * cheats.c — cheat engine for applying byte-stream memory patches to
 * running PS5 games.
 *
 * Architecture:
 *   - Three file formats: JSON (native), SHN (XML trainer), MC4 (base64 over
 *     AES-256-CBC, decrypting to the same XML as SHN). All parsed into a
 *     common in-memory representation.
 *   - Memory writes via ptrace (PT_ATTACH + PT_IO), with kernel_mprotect
 *     to flip execute-only pages to RWX temporarily.
 *   - A background watcher thread polls every 3s for new game processes
 *     and auto-applies "patches" (always-on) + re-applies user-toggled
 *     cheats.
 *   - Toggle state persisted to sidecar JSON files so it survives payload
 *     redeploy.
 *
 * Ported from elf-arsenal/src/cheats.c, adapted to ps5upload's ptrace
 * infrastructure (ptrace_remote.c) and FTX2 frame protocol.
 */

#include "cheats.h"
#include "notif.h"

#include <ps5/kernel.h>

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/sysctl.h>
#include <unistd.h>

#include "notif.h"
#include "proc_list.h"
#include "ptrace_remote.h"
#include "aes.h" /* tiny-AES, vendored — MC4 cheat decryption */

/* ── Constants ───────────────────────────────────────────────────── */

#define CHEATS_ROOT      "/data/ps5upload/cheats"
#define CHEATS_JSON_DIR  CHEATS_ROOT "/json"
#define CHEATS_SHN_DIR   CHEATS_ROOT "/shn"
#define CHEATS_MC4_DIR   CHEATS_ROOT "/mc4"
#define PATCHES_ROOT     "/data/ps5upload/patches"
#define PATCHES_JSON_DIR PATCHES_ROOT "/json"
#define PATCHES_SHN_DIR  PATCHES_ROOT "/shn"
#define PATCHES_MC4_DIR  PATCHES_ROOT "/mc4"
#define CHEATS_STATE_DIR CHEATS_ROOT "/state"

#define MAX_CHEAT_FILEPATH  300
#define MAX_TITLE_ID        16
#define MAX_CHEAT_NAME      256
#define MAX_MODS_PER_FILE   64
#define MAX_MEM_ENTRIES     32
#define MAX_HEX_BYTES       256
#define MAX_JSON_BUF        (256 * 1024)
#define CHEAT_FILE_MAX      (4 * 1024 * 1024)

#define RG_CACHE_TTL_MS     2000

#define KINFO_PID_OFFSET    72
#define KINFO_TDNAME_OFFSET 447
#define KINFO_STRUCT_MINSIZE 448

#define PROT_READ  0x01
#define PROT_WRITE 0x02
#define PROT_EXEC  0x04

#define PAGE_SIZE_16K 0x4000
#define ROUND_PG_DOWN(a) ((intptr_t)((a) & ~(intptr_t)(PAGE_SIZE_16K - 1)))
#define ROUND_PG_UP(a)   ((intptr_t)(((a) + PAGE_SIZE_16K - 1) & ~(intptr_t)(PAGE_SIZE_16K - 1)))

/* Canonical layout + title-id helpers. */
#include "app_info.h"

/* ── Engine state ────────────────────────────────────────────────── */

static atomic_int g_engine_enabled = 0;
static atomic_int g_patches_last = 0;
static atomic_int g_patches_total = 0;
static atomic_int g_watcher_running = 0;

/* Running-game cache */
static pthread_mutex_t g_rg_lock = PTHREAD_MUTEX_INITIALIZER;
static struct {
    int      valid;
    pid_t    pid;
    char     title[MAX_TITLE_ID];
    intptr_t base;
    struct timespec ts;
} g_rg_cache;

/* ── Hex parsing ─────────────────────────────────────────────────── */

static int hex_val(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Parse a hex string like "90 1F 00 00" or "901F0000" into bytes.
 * Separators (space, comma, dash, colon, tab, newline) are skipped.
 * Returns byte count, or -1 on invalid hex. */
static int parse_hex_bytes(const char *str, uint8_t *out, int max) {
    int n = 0;
    int hi = -1;
    for (const char *p = str; *p && n < max; p++) {
        if (*p == ' ' || *p == ',' || *p == '-' || *p == ':' ||
            *p == '\t' || *p == '\n' || *p == '\r') continue;
        int v = hex_val(*p);
        if (v < 0) return -1;
        if (hi < 0) {
            hi = v;
        } else {
            out[n++] = (uint8_t)(hi * 16 + v);
            hi = -1;
        }
    }
    if (hi >= 0) {
        /* dangling nibble — treat as low nibble of a byte */
        out[n++] = (uint8_t)(hi);
    }
    return n;
}

/* Parse offset string to intptr_t. Accepts bare hex or 0x-prefixed. */
static intptr_t parse_offset(const char *s) {
    if (!s || !*s) return 0;
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) s += 2;
    return (intptr_t)strtoull(s, NULL, 16);
}

/* ── JSON string extraction helpers (from runtime.c patterns) ────── */

/* Find the value of a string field in a JSON blob. Writes the unquoted,
 * unescaped value to out. Returns 0 if found, -1 if not. */
static int json_str(const char *json, const char *key, char *out, size_t cap) {
    if (!json || !key || !out || cap == 0) return -1;
    out[0] = '\0';
    size_t klen = strlen(key);
    const char *p = json;
    while ((p = strstr(p, key))) {
        /* Check this is a key: preceded by " and followed by " : */
        const char *q = p + klen;
        if (p > json && p[-1] == '"' && q[0] == '"' &&
            (q[1] == ':' || q[1] == ' ' || q[1] == '\t')) {
            /* Skip to the colon, then to the opening quote */
            p = q + 1;
            while (*p && *p != ':') p++;
            if (*p != ':') return -1;
            p++;
            while (*p == ' ' || *p == '\t') p++;
            if (*p != '"') return -1;
            p++;
            size_t i = 0;
            while (*p && *p != '"' && i + 1 < cap) {
                if (*p == '\\' && p[1]) {
                    switch (p[1]) {
                        case '"': out[i++] = '"'; break;
                        case '\\': out[i++] = '\\'; break;
                        case '/': out[i++] = '/'; break;
                        case 'n': out[i++] = '\n'; break;
                        case 't': out[i++] = '\t'; break;
                        case 'r': out[i++] = '\r'; break;
                        default: out[i++] = p[1]; break;
                    }
                    p += 2;
                } else {
                    out[i++] = *p++;
                }
            }
            out[i] = '\0';
            return 0;
        }
        p += klen;
    }
    return -1;
}

/* Find a numeric field in JSON. Returns 0 if found, -1 if not. */
static int json_num(const char *json, const char *key, int64_t *out) {
    if (!json || !key || !out) return -1;
    size_t klen = strlen(key);
    const char *p = json;
    while ((p = strstr(p, key))) {
        const char *q = p + klen;
        if (p > json && p[-1] == '"' && q[0] == '"' &&
            (q[1] == ':' || q[1] == ' ' || q[1] == '\t')) {
            p = q + 1;
            while (*p && *p != ':') p++;
            if (*p != ':') return -1;
            p++;
            while (*p == ' ' || *p == '\t') p++;
            char *end;
            *out = (int64_t)strtoll(p, &end, 10);
            if (end != p) return 0;
            return -1;
        }
        p += klen;
    }
    return -1;
}

/* Find a boolean field in JSON. Returns 0 if found, -1 if not. */
static int json_bool(const char *json, const char *key, int *out) {
    if (!json || !key || !out) return -1;
    size_t klen = strlen(key);
    const char *p = json;
    while ((p = strstr(p, key))) {
        const char *q = p + klen;
        if (p > json && p[-1] == '"' && q[0] == '"' &&
            (q[1] == ':' || q[1] == ' ' || q[1] == '\t')) {
            p = q + 1;
            while (*p && *p != ':') p++;
            if (*p != ':') return -1;
            p++;
            while (*p == ' ' || *p == '\t') p++;
            if (strncmp(p, "true", 4) == 0) { *out = 1; return 0; }
            if (strncmp(p, "false", 5) == 0) { *out = 0; return 0; }
            return -1;
        }
        p += klen;
    }
    return -1;
}

/* ── JSON string buffer (for building output) ────────────────────── */

typedef struct {
    char  *buf;
    size_t cap;
    size_t off;
} jbuf_t;

static void jb_init(jbuf_t *jb, char *buf, size_t cap) {
    jb->buf = buf;
    jb->cap = cap;
    jb->off = 0;
    if (cap > 0) buf[0] = '\0';
}

static void jb_str(jbuf_t *jb, const char *s) {
    if (!s) return;
    for (; *s && jb->off + 2 < jb->cap; s++) {
        char c = *s;
        if (c == '"' || c == '\\') {
            if (jb->off + 3 >= jb->cap) break;
            jb->buf[jb->off++] = '\\';
            jb->buf[jb->off++] = c;
        } else if (c == '\n') {
            if (jb->off + 3 >= jb->cap) break;
            jb->buf[jb->off++] = '\\';
            jb->buf[jb->off++] = 'n';
        } else if ((unsigned char)c < 0x20) {
            continue;
        } else {
            jb->buf[jb->off++] = c;
        }
    }
    jb->buf[jb->off] = '\0';
}

static void jb_raw(jbuf_t *jb, const char *s) {
    size_t len = strlen(s);
    if (jb->off + len >= jb->cap) len = jb->cap - jb->off - 1;
    memcpy(jb->buf + jb->off, s, len);
    jb->off += len;
    jb->buf[jb->off] = '\0';
}

#define JB_PRINTF(jb, ...) do { \
    char _tmp[512]; \
    int _n = snprintf(_tmp, sizeof(_tmp), __VA_ARGS__); \
    if (_n > 0) jb_raw((jb), _tmp); \
} while(0)

/* ── Running-game detection ──────────────────────────────────────── */

/* Find the pid + base address of the running big app (game).
 * Walks sysctl(KERN_PROC_PROC) and uses sceKernelGetAppInfo to find
 * a non-NPXS app. Returns pid (>0) on success, 0 if no game running.
 * Stores title_id and eboot base in the out params. */
static pid_t find_running_game(char *title_out, size_t title_cap,
                               intptr_t *base_out) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) return 0;
    if (buf_size == 0) return 0;

    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf) return 0;
    if (sysctl(mib, 4, buf, &buf_size, NULL, 0) != 0) {
        free(buf);
        return 0;
    }

    pid_t found = 0;
    for (uint8_t *ptr = buf; ptr < buf + buf_size && !found; ) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize < KINFO_STRUCT_MINSIZE ||
            (size_t)(ptr - buf) + (size_t)ki_structsize > buf_size) break;
        pid_t pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];
        app_info_t info;
        memset(&info, 0, sizeof(info));
        char tid[10];
        if (sceKernelGetAppInfo(pid, &info) == 0 &&
            info.app_id != 0 &&
            app_info_title_id(&info, tid, sizeof(tid)) &&
            strncmp(tid, "NPXS", 4) != 0) {
            found = pid;
            if (title_out && title_cap > 0) {
                snprintf(title_out, title_cap, "%s", tid);
            }
            if (base_out) {
                *base_out = kernel_dynlib_mapbase_addr(pid, 0);
            }
        }
        ptr += ki_structsize;
    }
    free(buf);
    return found;
}

/* Cached running-game lookup. elf-arsenal caches for 2s because the
 * sysctl + GetAppInfo walk is expensive and called from multiple paths. */
static pid_t get_running_game_cached(char *title_out, size_t title_cap,
                                     intptr_t *base_out) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);

    pthread_mutex_lock(&g_rg_lock);
    int64_t age_ms = (int64_t)(now.tv_sec - g_rg_cache.ts.tv_sec) * 1000 +
                     (int64_t)(now.tv_nsec - g_rg_cache.ts.tv_nsec) / 1000000;
    if (g_rg_cache.valid && age_ms < RG_CACHE_TTL_MS) {
        pid_t pid = g_rg_cache.pid;
        if (title_out && title_cap > 0)
            snprintf(title_out, title_cap, "%s", g_rg_cache.title);
        if (base_out) *base_out = g_rg_cache.base;
        pthread_mutex_unlock(&g_rg_lock);
        return pid;
    }
    pthread_mutex_unlock(&g_rg_lock);

    /* Cache miss — do the expensive lookup */
    char title[MAX_TITLE_ID] = "";
    intptr_t base = 0;
    pid_t pid = find_running_game(title, sizeof(title), &base);

    pthread_mutex_lock(&g_rg_lock);
    g_rg_cache.valid = 1;
    g_rg_cache.pid = pid;
    snprintf(g_rg_cache.title, sizeof(g_rg_cache.title), "%s", title);
    g_rg_cache.base = base;
    g_rg_cache.ts = now;
    pthread_mutex_unlock(&g_rg_lock);

    if (title_out && title_cap > 0)
        snprintf(title_out, title_cap, "%s", title);
    if (base_out) *base_out = base;
    return pid;
}

/* Invalidate the cache (used after toggles / reloads). */
static void invalidate_rg_cache(void) {
    pthread_mutex_lock(&g_rg_lock);
    g_rg_cache.valid = 0;
    pthread_mutex_unlock(&g_rg_lock);
}

/* ── Memory write via ptrace ─────────────────────────────────────── */

/* Write bytes into a target process at addr. Temporarily flips page
 * protection to RWX if needed, writes, verifies by readback, restores.
 * Returns 0 on success, -1 on write failure, -2 on readback failure,
 * -3 on verify mismatch. */
static int write_process_memory(pid_t pid, intptr_t addr,
                                const uint8_t *data, size_t len) {
    if (len == 0) return 0;

    intptr_t page = ROUND_PG_DOWN(addr);
    intptr_t span = ROUND_PG_UP(addr + len) - page;

    /* Flip to RWX for the write */
    if (kernel_mprotect(pid, page, span,
                        PROT_READ | PROT_WRITE | PROT_EXEC) != 0) {
        return -1;
    }

    /* Write */
    if (pt_copyin(pid, data, addr, len) != 0) {
        (void)kernel_mprotect(pid, page, span, PROT_READ | PROT_EXEC);
        return -1;
    }

    /* Verify by readback */
    uint8_t readback[MAX_HEX_BYTES];
    if (len > sizeof(readback)) len = sizeof(readback);
    if (pt_copyout(pid, addr, readback, len) != 0) {
        (void)kernel_mprotect(pid, page, span, PROT_READ | PROT_EXEC);
        return -2;
    }

    int ok = (memcmp(readback, data, len) == 0);

    /* Restore to R-X */
    (void)kernel_mprotect(pid, page, span, PROT_READ | PROT_EXEC);

    return ok ? 0 : -3;
}

/* Read bytes from target process, handling execute-only (XOM) pages. */
static int read_process_memory(pid_t pid, intptr_t addr,
                               uint8_t *out, size_t len) {
    if (len == 0) return 0;
    if (pt_copyout(pid, addr, out, len) == 0) return 0;

    /* pt_copyout can fail on XOM pages — temporarily add PROT_READ */
    intptr_t page = ROUND_PG_DOWN(addr);
    intptr_t span = ROUND_PG_UP(addr + len) - page;
    int orig_prot = kernel_get_vmem_protection(pid, page, span);
    if (orig_prot < 0) orig_prot = PROT_READ | PROT_EXEC;

    if (!(orig_prot & PROT_READ)) {
        (void)kernel_mprotect(pid, page, span,
                              (orig_prot | PROT_READ));
    }
    int rc = pt_copyout(pid, addr, out, len);
    if (!(orig_prot & PROT_READ)) {
        (void)kernel_mprotect(pid, page, span, orig_prot);
    }
    return rc;
}

/* ── Module base resolution ──────────────────────────────────────── */

/* Resolve the base address of a named module in the target process.
 * NULL or empty → eboot base (handle 0). */
static intptr_t resolve_module_base(pid_t pid, const char *module_name) {
    if (!module_name || !module_name[0]) {
        return kernel_dynlib_mapbase_addr(pid, 0);
    }
    uint32_t handle = 0;
    if (kernel_dynlib_handle(pid, module_name, &handle) != 0 || !handle) {
        return 0;
    }
    return kernel_dynlib_mapbase_addr(pid, handle);
}

/* ── In-memory mod representation ────────────────────────────────── */

typedef struct {
    char    offset_str[32];
    char    on_str[256];
    char    off_str[256];
    int     absolute;
    int     section;
} mem_entry_t;

typedef struct {
    char        name[MAX_CHEAT_NAME];
    char        hint[MAX_CHEAT_NAME];
    char        type[16];       /* "checkbox" or "button" */
    char        module_name[128];
    int         enabled;        /* persisted toggle state */
    mem_entry_t mem[MAX_MEM_ENTRIES];
    int         mem_count;
} cheat_mod_t;

typedef struct {
    char        title_id[MAX_TITLE_ID];
    char        game_name[MAX_CHEAT_NAME];
    char        process[128];
    char        filepath[MAX_CHEAT_FILEPATH];
    int         format;         /* 1=json, 2=shn, 3=mc4 */
    cheat_mod_t mods[MAX_MODS_PER_FILE];
    int         mod_count;
} cheat_file_t;

/* ── JSON parser (parses one cheat file from disk) ───────────────── */

/* Extract the mods array from a JSON cheat file. The JSON format is
 * the canonical internal representation:
 *   {"name":"...","id":"PPSA...","mods":[{"name":"...","type":"checkbox",
 *    "memory":[{"offset":"...","on":"...","off":"..."}], ...}], ...}
 * We parse it with string scanning, same approach as runtime.c. */
static int parse_json_file(const char *json, size_t json_len,
                           cheat_file_t *cf) {
    (void)json_len;
    /* Extract top-level fields */
    json_str(json, "id", cf->title_id, sizeof(cf->title_id));
    json_str(json, "name", cf->game_name, sizeof(cf->game_name));
    json_str(json, "process", cf->process, sizeof(cf->process));

    /* Find "mods":[ */
    const char *p = strstr(json, "\"mods\"");
    if (!p) return 0;
    p = strchr(p, '[');
    if (!p) return 0;
    p++;

    int mod_idx = 0;
    while (*p && mod_idx < MAX_MODS_PER_FILE) {
        /* Skip to next '{' */
        while (*p && *p != '{') p++;
        if (!*p || *p == ']') break;

        /* Find the matching close brace by depth counting */
        int depth = 0;
        const char *mod_start = p;
        while (*p) {
            if (*p == '{') depth++;
            else if (*p == '}') { depth--; if (depth == 0) break; }
            p++;
        }
        if (!*p) break;
        const char *mod_end = p + 1;

        /* Extract this mod's substring */
        size_t mod_len = (size_t)(mod_end - mod_start);
        char mod_buf[8192];
        if (mod_len >= sizeof(mod_buf)) mod_len = sizeof(mod_buf) - 1;
        memcpy(mod_buf, mod_start, mod_len);
        mod_buf[mod_len] = '\0';

        cheat_mod_t *m = &cf->mods[mod_idx];
        memset(m, 0, sizeof(*m));
        strcpy(m->type, "checkbox");

        json_str(mod_buf, "name", m->name, sizeof(m->name));
        json_str(mod_buf, "hint", m->hint, sizeof(m->hint));
        json_str(mod_buf, "type", m->type, sizeof(m->type));
        json_str(mod_buf, "module_name", m->module_name,
                 sizeof(m->module_name));

        int bval = 0;
        if (json_bool(mod_buf, "_sonic_enabled", &bval) == 0) {
            m->enabled = bval;
        }

        /* Parse memory array */
        const char *mp = strstr(mod_buf, "\"memory\"");
        if (mp) {
            mp = strchr(mp, '[');
            if (mp) {
                mp++;
                int mi = 0;
                while (*mp && mi < MAX_MEM_ENTRIES) {
                    while (*mp && *mp != '{') mp++;
                    if (!*mp || *mp == ']') break;

                    depth = 0;
                    const char *me_start = mp;
                    while (*mp) {
                        if (*mp == '{') depth++;
                        else if (*mp == '}') {
                            depth--;
                            if (depth == 0) break;
                        }
                        mp++;
                    }
                    if (!*mp) break;
                    const char *me_end = mp + 1;

                    size_t me_len = (size_t)(me_end - me_start);
                    char me_buf[2048];
                    if (me_len >= sizeof(me_buf))
                        me_len = sizeof(me_buf) - 1;
                    memcpy(me_buf, me_start, me_len);
                    me_buf[me_len] = '\0';

                    mem_entry_t *e = &m->mem[mi];
                    memset(e, 0, sizeof(*e));
                    json_str(me_buf, "offset", e->offset_str,
                             sizeof(e->offset_str));
                    json_str(me_buf, "on", e->on_str,
                             sizeof(e->on_str));
                    json_str(me_buf, "off", e->off_str,
                             sizeof(e->off_str));
                    int64_t sv = 0;
                    if (json_num(me_buf, "section", &sv) == 0)
                        e->section = (int)sv;
                    int bv = 0;
                    if (json_bool(me_buf, "absolute", &bv) == 0)
                        e->absolute = bv;

                    if (e->offset_str[0]) mi++;
                    mp = me_end;
                }
                m->mem_count = mi;
            }
        }

        mod_idx++;
        p = mod_end;
    }

    cf->mod_count = mod_idx;
    return mod_idx;
}

/* ── SHN (XML) parser ────────────────────────────────────────────── */

/* Tiny XML attribute extractor. Finds attr="value" in tag. */
static int xml_attr(const char *tag, const char *attr,
                    char *out, size_t cap) {
    if (!tag || !attr || !out || cap == 0) return -1;
    out[0] = '\0';
    size_t alen = strlen(attr);
    const char *p = tag;
    while ((p = strstr(p, attr))) {
        if ((size_t)(p - tag) >= alen || p == tag || p[-1] == ' ' ||
            p[-1] == '\t') {
            p += alen;
            while (*p == ' ' || *p == '\t' || *p == '=') p++;
            if (*p == '"') {
                p++;
                size_t i = 0;
                while (*p && *p != '"' && i + 1 < cap) {
                    out[i++] = *p++;
                }
                out[i] = '\0';
                return 0;
            }
        }
        p += alen;
    }
    return -1;
}

/* Extract text between <tag>...</tag>. */
static int xml_text(const char *start, const char *tag_name,
                    char *out, size_t cap) {
    if (!start || !tag_name || !out || cap == 0) return -1;
    out[0] = '\0';
    char open_tag[64];
    char close_tag[64];
    snprintf(open_tag, sizeof(open_tag), "<%s", tag_name);
    snprintf(close_tag, sizeof(close_tag), "</%s>", tag_name);

    const char *p = strstr(start, open_tag);
    if (!p) return -1;
    p += strlen(open_tag);
    while (*p && *p != '>') p++;  /* skip attrs */
    if (*p == '>') p++;

    const char *end = strstr(p, close_tag);
    if (!end) return -1;

    size_t len = (size_t)(end - p);
    if (len >= cap) len = cap - 1;
    memcpy(out, p, len);
    out[len] = '\0';

    /* Trim whitespace */
    while (len > 0 && isspace((unsigned char)out[len-1])) out[--len] = '\0';
    while (isspace((unsigned char)out[0])) memmove(out, out+1, strlen(out));

    return 0;
}

/* Find next occurrence of <TagName in xml text starting from cursor.
 * Returns pointer to the '<', or NULL. Updates *next_end to point past
 * the tag's '>' (self-closing or paired). */
static const char *xml_find_tag(const char *cursor, const char *tag_name,
                                const char **tag_start,
                                const char **tag_end) {
    char pattern[64];
    snprintf(pattern, sizeof(pattern), "<%s", tag_name);
    const char *p = strstr(cursor, pattern);
    if (!p) return NULL;
    *tag_start = p;
    const char *end = strchr(p, '>');
    if (!end) return NULL;
    *tag_end = end + 1;
    return p;
}

/* Parse SHN (XML trainer) format. The XML schema is:
 *   <Trainer Game="..." Version="...">
 *     <Cheat Text="Infinite Health" Description="...">
 *       <Cheatline>
 *         <Offset>01A4F800</Offset>
 *         <ValueOn>901F0000</ValueOn>
 *         <ValueOff>801F0000</ValueOff>
 *         <Absolute>false</Absolute>
 *         <Section>0</Section>
 *       </Cheatline>
 *     </Cheat>
 *   </Trainer> */
static int parse_shn_file(const char *xml, size_t xml_len,
                          cheat_file_t *cf) {
    (void)xml_len;
    cf->process[0] = '\0';

    /* Extract game name from <Trainer Game="..."> */
    const char *trainer = strstr(xml, "<Trainer");
    if (trainer) {
        xml_attr(trainer, "Game", cf->game_name, sizeof(cf->game_name));
        if (!cf->game_name[0])
            xml_attr(trainer, "GameName", cf->game_name,
                     sizeof(cf->game_name));
    }

    /* Iterate over <Cheat> tags */
    int mod_idx = 0;
    const char *cursor = xml;
    while (mod_idx < MAX_MODS_PER_FILE) {
        const char *cheat_start, *cheat_end;
        if (!xml_find_tag(cursor, "Cheat", &cheat_start, &cheat_end)) break;

        /* Extract the full Cheat tag content (to its matching </Cheat>) */
        const char *close_cheat = strstr(cheat_end, "</Cheat>");
        if (!close_cheat) break;

        size_t cheat_len = (size_t)(close_cheat - cheat_end);
        char cheat_buf[8192];
        if (cheat_len >= sizeof(cheat_buf)) cheat_len = sizeof(cheat_buf) - 1;
        memcpy(cheat_buf, cheat_end, cheat_len);
        cheat_buf[cheat_len] = '\0';

        cheat_mod_t *m = &cf->mods[mod_idx];
        memset(m, 0, sizeof(*m));
        strcpy(m->type, "checkbox");

        xml_attr(cheat_start, "Text", m->name, sizeof(m->name));
        if (!m->name[0])
            xml_attr(cheat_start, "CheatName", m->name, sizeof(m->name));
        if (!m->name[0])
            xml_attr(cheat_start, "Name", m->name, sizeof(m->name));

        char desc[256];
        if (xml_attr(cheat_start, "Description", desc, sizeof(desc)) == 0) {
            snprintf(m->hint, sizeof(m->hint), "%s", desc);
        }

        /* Iterate over <Cheatline> tags within this Cheat */
        int mi = 0;
        const char *cl_cursor = cheat_buf;
        while (mi < MAX_MEM_ENTRIES) {
            const char *cl_start, *cl_end;
            if (!xml_find_tag(cl_cursor, "Cheatline", &cl_start, &cl_end))
                break;

            const char *cl_close = strstr(cl_end, "</Cheatline>");
            const char *cl_real_end;
            if (cl_close) {
                cl_real_end = cl_close + strlen("</Cheatline>");
            } else {
                /* Self-closing: <Cheatline ... /> */
                cl_real_end = cl_end;
                cl_close = cl_start + strlen("<Cheatline");
            }

            size_t cl_len;
            if (cl_close && cl_close > cl_end) {
                cl_len = (size_t)(cl_close - cl_end);
            } else {
                cl_len = 0;
            }

            char cl_buf[2048];
            if (cl_len >= sizeof(cl_buf)) cl_len = sizeof(cl_buf) - 1;
            memcpy(cl_buf, cl_end, cl_len);
            cl_buf[cl_len] = '\0';

            mem_entry_t *e = &m->mem[mi];
            memset(e, 0, sizeof(*e));

            /* For self-closing tags, extract attributes directly */
            char cl_full[2048];
            size_t full_len = (size_t)(cl_real_end - cl_start);
            if (full_len >= sizeof(cl_full)) full_len = sizeof(cl_full) - 1;
            memcpy(cl_full, cl_start, full_len);
            cl_full[full_len] = '\0';

            /* Try attributes first (self-closing form) */
            xml_attr(cl_full, "Offset", e->offset_str, sizeof(e->offset_str));
            xml_attr(cl_full, "ValueOn", e->on_str, sizeof(e->on_str));
            xml_attr(cl_full, "ValueOff", e->off_str, sizeof(e->off_str));

            char abs_str[8] = "";
            xml_attr(cl_full, "Absolute", abs_str, sizeof(abs_str));
            if (abs_str[0] == 't' || abs_str[0] == 'T' || abs_str[0] == '1')
                e->absolute = 1;

            char sec_str[8] = "";
            xml_attr(cl_full, "Section", sec_str, sizeof(sec_str));
            if (sec_str[0]) e->section = atoi(sec_str);

            /* Try child tags (paired form) */
            if (!e->offset_str[0])
                xml_text(cl_full, "Offset", e->offset_str, sizeof(e->offset_str));
            if (!e->on_str[0])
                xml_text(cl_full, "ValueOn", e->on_str, sizeof(e->on_str));
            if (!e->off_str[0])
                xml_text(cl_full, "ValueOff", e->off_str, sizeof(e->off_str));

            if (!e->absolute) {
                char tmp[8] = "";
                xml_text(cl_full, "Absolute", tmp, sizeof(tmp));
                if (tmp[0] == 't' || tmp[0] == 'T' || tmp[0] == '1')
                    e->absolute = 1;
            }
            if (!e->section) {
                char tmp[8] = "";
                xml_text(cl_full, "Section", tmp, sizeof(tmp));
                if (tmp[0]) e->section = atoi(tmp);
            }

            if (e->offset_str[0]) mi++;
            cl_cursor = cl_real_end;
        }
        m->mem_count = mi;

        if (m->name[0] || mi > 0) mod_idx++;
        cursor = close_cheat + strlen("</Cheat>");
    }

    cf->mod_count = mod_idx;
    return mod_idx;
}

/* ── File discovery ──────────────────────────────────────────────── */

/* Check if filename starts with <title_id> followed by . or _ */
static int filename_matches_title(const char *filename, const char *title_id) {
    size_t tlen = strlen(title_id);
    if (strncasecmp(filename, title_id, tlen) != 0) return 0;
    char next = filename[tlen];
    return (next == '.' || next == '_');
}

typedef struct {
    char path[MAX_CHEAT_FILEPATH];
    int  format;  /* 1=json, 2=shn, 3=mc4 */
} found_file_t;

/* Scan all cheat directories for files matching the title_id.
 * Returns count found (0 to N). */
static int find_cheat_files(const char *title_id,
                            found_file_t *out, int max_out,
                            int is_patches) {
    const char *json_dir = is_patches ? PATCHES_JSON_DIR : CHEATS_JSON_DIR;
    const char *shn_dir = is_patches ? PATCHES_SHN_DIR : CHEATS_SHN_DIR;
    const char *mc4_dir = is_patches ? PATCHES_MC4_DIR : CHEATS_MC4_DIR;
    const char *dirs[] = {json_dir, shn_dir, mc4_dir};
    int formats[] = {1, 2, 3};
    int count = 0;

    for (int d = 0; d < 3 && count < max_out; d++) {
        DIR *dir = opendir(dirs[d]);
        if (!dir) continue;
        struct dirent *de;
        while ((de = readdir(dir)) && count < max_out) {
            if (de->d_name[0] == '.') continue;
            const char *ext = strrchr(de->d_name, '.');
            if (!ext) continue;

            int fmt = 0;
            if (formats[d] == 1 && strcasecmp(ext, ".json") == 0) fmt = 1;
            else if (formats[d] == 2 && strcasecmp(ext, ".shn") == 0) fmt = 2;
            else if (formats[d] == 3 && strcasecmp(ext, ".mc4") == 0) fmt = 3;
            if (!fmt) continue;

            if (!filename_matches_title(de->d_name, title_id)) continue;

            snprintf(out[count].path, sizeof(out[count].path),
                     "%s/%s", dirs[d], de->d_name);
            out[count].format = fmt;
            count++;
        }
        closedir(dir);
    }
    return count;
}

/* ── MC4 (encrypted XML) ─────────────────────────────────────────────
 *
 * An MC4 file is base64 text of an AES-256-CBC ciphertext; decrypted it is
 * the same <Trainer>/<Cheat>/<Cheatline> XML as SHN, so it is handed to the
 * SHN parser once decrypted. The key and IV are fixed constants baked into
 * the format (public, not console-derived), so no device secrets are needed.
 * Recovered from OnionHEN's mc4_cheat_parser and verified against its fixture.
 */
/* The 32-byte key and 16-byte IV are the ASCII bytes of these literals; the
 * arrays are auto-sized (a trailing NUL rides along, which AES never reads). */
static const uint8_t MC4_AES_KEY[] = "304c6528f659c766110239a51cl5dd9c";
static const uint8_t MC4_AES_IV[] = "u@}kzW2u[u(8DWar";

/* Standard base64 decode. Ignores whitespace; returns bytes written, or -1
 * on a malformed character. `out` must hold at least `3*(in_len/4)` bytes. */
static int mc4_base64_decode(const char *in, size_t in_len, uint8_t *out, size_t out_cap) {
    static const int8_t T[256] = {
        ['A']=0,['B']=1,['C']=2,['D']=3,['E']=4,['F']=5,['G']=6,['H']=7,['I']=8,
        ['J']=9,['K']=10,['L']=11,['M']=12,['N']=13,['O']=14,['P']=15,['Q']=16,
        ['R']=17,['S']=18,['T']=19,['U']=20,['V']=21,['W']=22,['X']=23,['Y']=24,
        ['Z']=25,['a']=26,['b']=27,['c']=28,['d']=29,['e']=30,['f']=31,['g']=32,
        ['h']=33,['i']=34,['j']=35,['k']=36,['l']=37,['m']=38,['n']=39,['o']=40,
        ['p']=41,['q']=42,['r']=43,['s']=44,['t']=45,['u']=46,['v']=47,['w']=48,
        ['x']=49,['y']=50,['z']=51,['0']=52,['1']=53,['2']=54,['3']=55,['4']=56,
        ['5']=57,['6']=58,['7']=59,['8']=60,['9']=61,['+']=62,['/']=63,
    };
    size_t o = 0;
    uint32_t acc = 0;
    int bits = 0;
    for (size_t i = 0; i < in_len; i++) {
        unsigned char c = (unsigned char)in[i];
        if (c == '=' ) break;
        if (c == '\r' || c == '\n' || c == ' ' || c == '\t') continue;
        int8_t v = T[c];
        if (v < 0 && c != 'A') return -1; /* 'A' legitimately maps to 0 */
        acc = (acc << 6) | (uint32_t)v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            if (o >= out_cap) return -1;
            out[o++] = (uint8_t)((acc >> bits) & 0xFF);
        }
    }
    return (int)o;
}

/* Decrypt an MC4 file in place-ish and parse it as SHN XML. */
static int parse_mc4_file(const char *encoded, size_t enc_len, cheat_file_t *cf) {
    /* base64 → binary */
    size_t cap = (enc_len / 4 + 1) * 3 + 16;
    uint8_t *bin = (uint8_t *)malloc(cap);
    if (!bin) return -1;
    int bin_len = mc4_base64_decode(encoded, enc_len, bin, cap);
    /* AES-256-CBC works on whole 16-byte blocks. */
    if (bin_len <= 0 || (bin_len % 16) != 0) {
        free(bin);
        return -1;
    }

    struct AES_ctx ctx;
    AES_init_ctx_iv(&ctx, MC4_AES_KEY, MC4_AES_IV);
    AES_CBC_decrypt_buffer(&ctx, bin, (size_t)bin_len);

    /* The plaintext is NUL-terminated XML; PKCS pad bytes at the tail are
     * harmless — the SHN parser stops at the closing tags. */
    char *xml = (char *)realloc(bin, (size_t)bin_len + 1);
    if (!xml) { free(bin); return -1; }
    xml[bin_len] = '\0';

    int rc = parse_shn_file(xml, (size_t)bin_len, cf);
    free(xml);
    return rc;
}

/* Load and parse a cheat file from disk. Returns 0 on success. */
static int load_cheat_file(const char *path, int format, cheat_file_t *cf) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size > CHEAT_FILE_MAX) {
        close(fd);
        return -1;
    }
    size_t fsize = (size_t)st.st_size;
    char *buf = (char *)malloc(fsize + 1);
    if (!buf) { close(fd); return -1; }
    ssize_t rd = read(fd, buf, fsize);
    close(fd);
    if (rd < 0) { free(buf); return -1; }
    buf[rd] = '\0';

    memset(cf, 0, sizeof(*cf));
    snprintf(cf->filepath, sizeof(cf->filepath), "%s", path);
    cf->format = format;

    int rc = -1;
    if (format == 1) {
        rc = parse_json_file(buf, (size_t)rd, cf);
    } else if (format == 2) {
        rc = parse_shn_file(buf, (size_t)rd, cf);
    } else if (format == 3) {
        /* MC4 = base64 + AES-256-CBC over SHN-shaped XML. */
        rc = parse_mc4_file(buf, (size_t)rd, cf);
    }

    free(buf);
    return (rc >= 0) ? 0 : -1;
}

/* ── State persistence (sidecar JSON) ────────────────────────────── */

static void state_path(const char *title_id, char *out, size_t cap) {
    snprintf(out, cap, "%s/%s.json", CHEATS_STATE_DIR, title_id);
}

/* Load enabled state from sidecar. Updates cf->mods[].enabled. */
static void load_state(const char *title_id, cheat_file_t *cf) {
    char path[MAX_CHEAT_FILEPATH];
    state_path(title_id, path, sizeof(path));
    int fd = open(path, O_RDONLY);
    if (fd < 0) return;
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size > 4096) { close(fd); return; }
    char buf[4096];
    ssize_t rd = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (rd <= 0) return;
    buf[rd] = '\0';

    /* Parse the flat index keys: {"0":true,"3":true, ...}
     * These are flat indices across all files, but we only support
     * one file at a time here, so they map directly to cf->mods[]. */
    for (int i = 0; i < cf->mod_count; i++) {
        char key[16];
        snprintf(key, sizeof(key), "\"%d\"", i);
        const char *p = strstr(buf, key);
        if (p) {
            p += strlen(key);
            while (*p && *p != ':') p++;
            if (*p == ':') {
                p++;
                while (*p == ' ' || *p == '\t' || *p == '\n') p++;
                if (strncmp(p, "true", 4) == 0) {
                    cf->mods[i].enabled = 1;
                }
            }
        }
    }
}

/* Save enabled state to sidecar. */
static void save_state(const char *title_id, cheat_file_t *files,
                       int n_files) {
    char path[MAX_CHEAT_FILEPATH];
    state_path(title_id, path, sizeof(path));

    char tmp[MAX_CHEAT_FILEPATH];
    snprintf(tmp, sizeof(tmp), "%s.tmp", path);

    FILE *f = fopen(tmp, "w");
    if (!f) return;

    fprintf(f, "{");
    int first = 1;
    int flat = 0;
    for (int fi = 0; fi < n_files; fi++) {
        for (int mi = 0; mi < files[fi].mod_count; mi++) {
            if (files[fi].mods[mi].enabled) {
                if (!first) fprintf(f, ",");
                fprintf(f, "\"%d\":true", flat);
                first = 0;
            }
            flat++;
        }
    }
    fprintf(f, "}");

    if (fclose(f) == 0) {
        rename(tmp, path);
    } else {
        unlink(tmp);
    }
}

/* ── Core: apply a single mod ────────────────────────────────────── */

static int apply_mod(pid_t pid, intptr_t base, cheat_mod_t *mod,
                     int turn_on, char *err, size_t err_cap) {
    if (mod->mem_count == 0) {
        if (err) snprintf(err, err_cap, "mod has no memory entries");
        return -1;
    }

    /* "button" type always writes "on" regardless of turn_on */
    int write_on = (strcmp(mod->type, "button") == 0) ? 1 : turn_on;

    int wrote = 0;
    int skipped = 0;

    for (int i = 0; i < mod->mem_count; i++) {
        mem_entry_t *e = &mod->mem[i];
        if (!e->offset_str[0] || !e->on_str[0]) {
            skipped++;
            continue;
        }

        intptr_t addr;
        if (e->absolute) {
            addr = parse_offset(e->offset_str);
        } else if (e->section > 0) {
            intptr_t sec_base = kernel_dynlib_mapbase_addr(pid, (uint32_t)e->section);
            addr = sec_base + parse_offset(e->offset_str);
        } else {
            addr = base + parse_offset(e->offset_str);
        }

        /* Determine which bytes to write */
        const char *write_hex = write_on ? e->on_str : e->off_str;
        if (!write_hex[0]) write_hex = e->on_str;

        uint8_t write_bytes[MAX_HEX_BYTES];
        int wlen = parse_hex_bytes(write_hex, write_bytes, sizeof(write_bytes));
        if (wlen <= 0) {
            skipped++;
            continue;
        }

        /* Baseline guard: read current bytes and verify they match
         * either the "on" or "off" pattern. This prevents crashes from
         * wrong-game-build cheats. */
        uint8_t current[MAX_HEX_BYTES];
        memset(current, 0, sizeof(current));

        /* Parse on/off patterns for comparison */
        uint8_t on_bytes[MAX_HEX_BYTES], off_bytes[MAX_HEX_BYTES];
        int on_len = e->on_str[0] ?
            parse_hex_bytes(e->on_str, on_bytes, sizeof(on_bytes)) : 0;
        int off_len = e->off_str[0] ?
            parse_hex_bytes(e->off_str, off_bytes, sizeof(off_bytes)) : 0;

        /* Read current bytes at the address */
        int read_ok = (read_process_memory(pid, addr, current,
                                           (size_t)(on_len > 0 ? on_len : wlen)) == 0);
        if (read_ok) {
            int matches_on = (on_len > 0 &&
                              memcmp(current, on_bytes, (size_t)on_len) == 0);
            int matches_off = (off_len > 0 &&
                               memcmp(current, off_bytes, (size_t)off_len) == 0);
            if (!matches_on && !matches_off) {
                /* Baseline mismatch — wrong game build or wrong base */
                skipped++;
                continue;
            }
        }

        /* Write the bytes */
        int rc = write_process_memory(pid, addr, write_bytes, (size_t)wlen);
        if (rc == 0) {
            wrote++;
        } else {
            /* Write failed — don't abort, try remaining entries */
            skipped++;
        }
    }

    if (wrote > 0) {
        return 0;
    }

    if (skipped > 0 && err) {
        snprintf(err, err_cap,
                 "cheat does not match this game build "
                 "(%d location(s) skipped)", skipped);
    } else if (err) {
        snprintf(err, err_cap, "no writable memory entries");
    }
    return -1;
}

/* ── Background watcher thread ───────────────────────────────────── */

/* Is this cheat file's "name" actually a game's name?
 *
 * Measured against the published repos: plenty of cheat files carry a title id
 * ("PPSA21564") or a content id ("PPSA23226_00-GAME000000000000") in the name
 * field. Announcing "1 cheat applied to PPSA23226_00-GAME000000000000" on the
 * player's TV is worse than announcing the title id, because it reads as
 * corruption rather than as a missing name. The client applies the same test
 * to what it displays. */
static int is_usable_game_name(const char *name, const char *title_id) {
    if (!name || !name[0]) return 0;
    /* A bare title id tells the player nothing they do not have already. */
    if (title_id && title_id[0] && strcasecmp(name, title_id) == 0) return 0;
    /* Content ids embed the title id followed by '_' or '-'. */
    if (title_id && title_id[0]) {
        size_t n = strlen(title_id);
        if (strncasecmp(name, title_id, n) == 0 &&
            (name[n] == '_' || name[n] == '-')) return 0;
    }
    return 1;
}

/* Returns how many memory writes actually landed, and fills `name_out` with
 * the game's own name when a cheat file carries one — the console
 * notification reads far better as the game's title than as a title id. */
static int apply_patches_for_game(pid_t pid, intptr_t base,
                                  const char *title_id,
                                  char *name_out, size_t name_cap) {
    found_file_t files[16];
    int n = find_cheat_files(title_id, files, 16, 1 /* patches */);

    int total_writes = 0;
    for (int i = 0; i < n; i++) {
        cheat_file_t *cf = (cheat_file_t *)malloc(sizeof(cheat_file_t));
        if (!cf) continue;
        if (load_cheat_file(files[i].path, files[i].format, cf) != 0) {
            free(cf);
            continue;
        }

        if (pt_attach(pid) != 0) {
            free(cf);
            return total_writes;
        }
        if (name_out && name_cap && !name_out[0] &&
            is_usable_game_name(cf->game_name, title_id)) {
            snprintf(name_out, name_cap, "%s", cf->game_name);
        }
        for (int m = 0; m < cf->mod_count; m++) {
            char err[128];
            /* Patches are always-on: turn_on = 1 */
            if (apply_mod(pid, base, &cf->mods[m], 1, err, sizeof(err)) == 0) {
                total_writes++;
            }
        }
        pt_detach(pid, 0);
        free(cf);
    }
    atomic_store(&g_patches_last, n);
    atomic_fetch_add(&g_patches_total, total_writes);
    return total_writes;
}

/* Returns how many enabled cheats were re-applied. See the note above on
 * `name_out`. */
static int reapply_enabled_for_game(pid_t pid, intptr_t base,
                                    const char *title_id,
                                    char *name_out, size_t name_cap) {
    found_file_t files[16];
    int n = find_cheat_files(title_id, files, 16, 0 /* cheats */);

    for (int i = 0; i < n; i++) {
        cheat_file_t *cf = (cheat_file_t *)malloc(sizeof(cheat_file_t));
        if (!cf) continue;
        if (load_cheat_file(files[i].path, files[i].format, cf) != 0) {
            free(cf);
            continue;
        }
        load_state(title_id, cf);

        int has_enabled = 0;
        for (int m = 0; m < cf->mod_count; m++) {
            if (cf->mods[m].enabled) { has_enabled = 1; break; }
        }
        if (!has_enabled) { free(cf); continue; }

        if (pt_attach(pid) != 0) { free(cf); return 0; }
        if (name_out && name_cap && !name_out[0] &&
            is_usable_game_name(cf->game_name, title_id)) {
            snprintf(name_out, name_cap, "%s", cf->game_name);
        }
        int applied = 0;
        for (int m = 0; m < cf->mod_count; m++) {
            if (cf->mods[m].enabled) {
                char err[128];
                if (apply_mod(pid, base, &cf->mods[m], 1, err, sizeof(err)) == 0) {
                    applied++;
                }
            }
        }
        pt_detach(pid, 0);
        free(cf);
        return applied; /* Only process the first matching file */
    }
    return 0;
}

static void *watcher_thread(void *arg) {
    (void)arg;
    pid_t last_pid = 0;

    while (1) {
        sleep(3);

        if (!atomic_load(&g_engine_enabled)) {
            last_pid = 0;
            continue;
        }

        char title[MAX_TITLE_ID] = "";
        intptr_t base = 0;
        pid_t pid = get_running_game_cached(title, sizeof(title), &base);

        if (pid <= 0) {
            last_pid = 0;
            continue;
        }

        if (pid == last_pid) continue;
        last_pid = pid;

        invalidate_rg_cache();

        /* Auto-apply patches + re-apply user-toggled cheats */
        char game_name[MAX_CHEAT_NAME] = "";
        int applied = apply_patches_for_game(pid, base, title,
                                             game_name, sizeof(game_name));
        applied += reapply_enabled_for_game(pid, base, title,
                                            game_name, sizeof(game_name));

        /* Tell the player on the console. Without this the engine works
         * silently and there is no way to know a cheat actually took — the
         * request most asked for in issue #315.
         *
         * Fired here rather than per mod: this block runs once per game
         * session (the `pid == last_pid` guard above), so the player gets one
         * notification when the game starts rather than a burst of them. Only
         * when something actually landed — announcing "0 cheats" on every
         * launch of every game would be noise. */
        if (applied > 0) {
            char msg[256];
            snprintf(msg, sizeof(msg), "%d cheat%s applied to %s",
                     applied, applied == 1 ? "" : "s",
                     game_name[0] ? game_name : title);
            notif_send(msg, NOTIF_LEVEL_INFO);
        }
    }
    return NULL;
}

/* ── Initialization ──────────────────────────────────────────────── */

static void ensure_dir(const char *path) {
    mkdir(path, 0755);
}

void cheats_init(void) {
    ensure_dir(CHEATS_ROOT);
    ensure_dir(CHEATS_JSON_DIR);
    ensure_dir(CHEATS_SHN_DIR);
    ensure_dir(CHEATS_MC4_DIR);
    ensure_dir(PATCHES_ROOT);
    ensure_dir(PATCHES_JSON_DIR);
    ensure_dir(PATCHES_SHN_DIR);
    ensure_dir(PATCHES_MC4_DIR);
    ensure_dir(CHEATS_STATE_DIR);

    /* Start watcher thread if not already running */
    int expected = 0;
    if (atomic_compare_exchange_strong(&g_watcher_running, &expected, 1)) {
        pthread_t thr;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
        if (pthread_create(&thr, &attr, watcher_thread, NULL) == 0) {
            /* Thread started */
        } else {
            atomic_store(&g_watcher_running, 0);
        }
        pthread_attr_destroy(&attr);
    }
}

/* ── Engine flag ─────────────────────────────────────────────────── */

int cheats_engine_enabled(void) {
    return atomic_load(&g_engine_enabled);
}

void cheats_engine_set_enabled(int on) {
    atomic_store(&g_engine_enabled, on ? 1 : 0);
}

int cheats_patches_last_mod_count(void) {
    return atomic_load(&g_patches_last);
}

int cheats_patches_total_writes(void) {
    return atomic_load(&g_patches_total);
}

/* ── Public API implementations ──────────────────────────────────── */

/* List all titles that have cheat files. */
int cheats_list_titles(char *buf, size_t cap, size_t *written) {
    if (!buf || cap == 0) return -1;
    /* Running game info (resolve once so we can mark the active title) */
    char rg_title[MAX_TITLE_ID] = "";
    intptr_t rg_base = 0;
    pid_t rg_pid = get_running_game_cached(rg_title, sizeof(rg_title), &rg_base);

    jbuf_t jb;
    jb_init(&jb, buf, cap);

    jb_raw(&jb, "{\"titles\":[");

    const char *dirs[] = {CHEATS_JSON_DIR, CHEATS_SHN_DIR, CHEATS_MC4_DIR};
    int first = 1;
    char seen_titles[256][MAX_TITLE_ID];
    int seen_count = 0;

    for (int d = 0; d < 3; d++) {
        DIR *dir = opendir(dirs[d]);
        if (!dir) continue;
        struct dirent *de;
        while ((de = readdir(dir))) {
            if (de->d_name[0] == '.') continue;

            /* Extract title_id from filename (up to first . or _) */
            char title[MAX_TITLE_ID] = "";
            size_t i = 0;
            while (de->d_name[i] && de->d_name[i] != '.' &&
                   de->d_name[i] != '_' && i < sizeof(title) - 1) {
                title[i] = de->d_name[i];
                i++;
            }
            title[i] = '\0';
            if (i < 4) continue;  /* too short to be a real title id */

            /* Check if we already listed this title */
            int dup = 0;
            for (int s = 0; s < seen_count; s++) {
                if (strcasecmp(seen_titles[s], title) == 0) {
                    dup = 1;
                    break;
                }
            }
            if (dup) continue;
            if (seen_count < 256) {
                snprintf(seen_titles[seen_count++], MAX_TITLE_ID, "%s", title);
            }

            int is_running = (rg_pid > 0 &&
                              strcasecmp(rg_title, title) == 0);

            if (!first) jb_raw(&jb, ",");
            JB_PRINTF(&jb, "{\"title_id\":\"%s\",\"name\":\"%s\",\"running\":%s}",
                      title, title,
                      is_running ? "true" : "false");
            first = 0;
        }
        closedir(dir);
    }

    JB_PRINTF(&jb, "],\"game_running\":%s,\"game_title_id\":\"%s\"}",
              rg_pid > 0 ? "true" : "false", rg_title);

    if (written) *written = jb.off;
    return 0;
}

/* List all mods for a title. */
int cheats_list_mods(const char *title_id, char *buf, size_t cap,
                     size_t *written) {
    if (!title_id || !buf || cap == 0) return -1;

    found_file_t files[16];
    int n = find_cheat_files(title_id, files, 16, 0);
    if (n == 0) {
        const char *resp = "{\"mods\":[],\"error\":\"no cheat files found\"}";
        size_t len = strlen(resp);
        if (len >= cap) len = cap - 1;
        memcpy(buf, resp, len);
        buf[len] = '\0';
        if (written) *written = len;
        return 0;
    }

    jbuf_t jb;
    jb_init(&jb, buf, cap);
    jb_raw(&jb, "{\"title_id\":\"");
    jb_str(&jb, title_id);
    jb_raw(&jb, "\",\"mods\":[");

    int flat = 0;
    int first = 1;

    for (int fi = 0; fi < n; fi++) {
        cheat_file_t *cf = (cheat_file_t *)malloc(sizeof(cheat_file_t));
        if (!cf) continue;
        if (load_cheat_file(files[fi].path, files[fi].format, cf) != 0) {
            free(cf);
            continue;
        }
        load_state(title_id, cf);

        for (int mi = 0; mi < cf->mod_count; mi++) {
            cheat_mod_t *m = &cf->mods[mi];
            if (!first) jb_raw(&jb, ",");
            first = 0;

            JB_PRINTF(&jb, "{\"index\":%d,\"name\":\"", flat);
            jb_str(&jb, m->name);
            JB_PRINTF(&jb, "\",\"desc\":\"");
            jb_str(&jb, m->hint);
            JB_PRINTF(&jb, "\",\"type\":\"%s\",\"on\":%s}",
                      m->type,
                      m->enabled ? "true" : "false");
            flat++;
        }
        free(cf);
    }

    jb_raw(&jb, "]}");

    if (written) *written = jb.off;
    return 0;
}

/* Toggle a mod by flat index. */
int cheats_toggle(const char *title_id, int mod_index, int turn_on,
                  char *err, size_t err_cap) {
    if (!title_id) {
        if (err) snprintf(err, err_cap, "missing title_id");
        return -1;
    }

    /* Auto-enable engine if toggling while game is running */
    if (turn_on && !atomic_load(&g_engine_enabled)) {
        atomic_store(&g_engine_enabled, 1);
    }

    found_file_t files[16];
    int n = find_cheat_files(title_id, files, 16, 0);
    if (n == 0) {
        if (err) snprintf(err, err_cap,
                          "no cheat file for %s. Drop a .json/.shn into "
                          CHEATS_JSON_DIR " or " CHEATS_SHN_DIR,
                          title_id);
        return -1;
    }

    /* Find the running game */
    char rg_title[MAX_TITLE_ID] = "";
    intptr_t base = 0;
    pid_t pid = get_running_game_cached(rg_title, sizeof(rg_title), &base);
    if (pid <= 0) {
        if (err) snprintf(err, err_cap, "no game is currently running");
        return -1;
    }
    if (strcasecmp(rg_title, title_id) != 0) {
        if (err) snprintf(err, err_cap,
                          "running game (%s) does not match cheat target (%s)",
                          rg_title, title_id);
        return -1;
    }

    /* Resolve flat index to file + mod */
    int flat = 0;
    int target_fi = -1, target_mi = -1;
    cheat_file_t *target_cf = (cheat_file_t *)malloc(sizeof(cheat_file_t));
    if (!target_cf) {
        if (err) snprintf(err, err_cap, "out of memory");
        return -1;
    }
    memset(target_cf, 0, sizeof(*target_cf));

    for (int fi = 0; fi < n && target_fi < 0; fi++) {
        cheat_file_t *cf = (cheat_file_t *)malloc(sizeof(cheat_file_t));
        if (!cf) continue;
        if (load_cheat_file(files[fi].path, files[fi].format, cf) != 0) {
            free(cf);
            continue;
        }
        for (int mi = 0; mi < cf->mod_count; mi++) {
            if (flat == mod_index) {
                target_fi = fi;
                target_mi = mi;
                memcpy(target_cf, cf, sizeof(*target_cf));
                break;
            }
            flat++;
        }
        free(cf);
    }

    if (target_fi < 0) {
        free(target_cf);
        if (err) snprintf(err, err_cap, "cheat index out of range");
        return -1;
    }

    cheat_mod_t *mod = &target_cf->mods[target_mi];

    /* Resolve module base */
    intptr_t mod_base = base;
    if (mod->module_name[0]) {
        mod_base = resolve_module_base(pid, mod->module_name);
        if (!mod_base) {
            free(target_cf);
            if (err) snprintf(err, err_cap,
                              "module \"%s\" is not loaded in the target process",
                              mod->module_name);
            return -1;
        }
    }

    /* Attach and apply */
    if (pt_attach(pid) != 0) {
        free(target_cf);
        if (err) snprintf(err, err_cap, "pt_attach failed (errno=%d)", errno);
        return -1;
    }

    int rc = apply_mod(pid, mod_base, mod, turn_on, err, err_cap);

    pt_detach(pid, 0);

    if (rc == 0) {
        /* Update persisted state (only for checkbox type) */
        if (strcmp(mod->type, "button") != 0) {
            mod->enabled = turn_on;
            save_state(title_id, target_cf, 1);
        }

        /* Confirm on the console.
         *
         * This is the moment issue #315 actually describes — "you load a
         * cheat, it works in-game, but there is no notification". Toggling
         * requires a running game and applies the patch live, so it is the
         * one place the player is looking at the TV rather than the app.
         * Naming the cheat matters more than naming the game here: the
         * player already knows what they are playing, and what they need
         * confirmed is WHICH cheat took. */
        char msg[256];
        const char *label = mod->name[0] ? mod->name : "Cheat";
        if (strcmp(mod->type, "button") == 0) {
            snprintf(msg, sizeof(msg), "%s applied", label);
        } else {
            snprintf(msg, sizeof(msg), "%s %s", label, turn_on ? "on" : "off");
        }
        notif_send(msg, NOTIF_LEVEL_INFO);
    }

    free(target_cf);
    invalidate_rg_cache();
    return rc;
}

/* Delete all cheat files for a title. */
int cheats_delete(const char *title_id, char *err, size_t err_cap) {
    if (!title_id) {
        if (err) snprintf(err, err_cap, "missing title_id");
        return -1;
    }

    found_file_t files[16];
    int n = find_cheat_files(title_id, files, 16, 0);
    if (n == 0) {
        if (err) snprintf(err, err_cap, "no cheat files found for %s", title_id);
        return -1;
    }

    for (int i = 0; i < n; i++) {
        unlink(files[i].path);
    }

    /* Also delete sidecar state */
    char path[MAX_CHEAT_FILEPATH];
    state_path(title_id, path, sizeof(path));
    unlink(path);

    return 0;
}

/* Force re-apply. */
int cheats_reload(char *err, size_t err_cap) {
    (void)err;
    (void)err_cap;

    invalidate_rg_cache();

    char title[MAX_TITLE_ID] = "";
    intptr_t base = 0;
    pid_t pid = get_running_game_cached(title, sizeof(title), &base);
    if (pid <= 0) {
        if (err) snprintf(err, err_cap, "no game is currently running");
        return -1;
    }

    char game_name[MAX_CHEAT_NAME] = "";
    int applied = apply_patches_for_game(pid, base, title,
                                         game_name, sizeof(game_name));
    applied += reapply_enabled_for_game(pid, base, title,
                                        game_name, sizeof(game_name));

    /* A reload is something the player asked for, so confirm it on the
     * console the same way an automatic apply does. Saying so when nothing
     * landed matters here — unlike the watcher, a silent reload leaves the
     * player unsure whether they had toggled anything at all. */
    char msg[256];
    if (applied > 0) {
        snprintf(msg, sizeof(msg), "%d cheat%s applied to %s",
                 applied, applied == 1 ? "" : "s",
                 game_name[0] ? game_name : title);
    } else {
        snprintf(msg, sizeof(msg), "No cheats enabled for %s",
                 game_name[0] ? game_name : title);
    }
    notif_send(msg, NOTIF_LEVEL_INFO);

    return 0;
}

/* Engine status JSON. */
int cheats_status_json(char *buf, size_t cap, size_t *written) {
    if (!buf || cap == 0) return -1;

    char rg_title[MAX_TITLE_ID] = "";
    intptr_t rg_base = 0;
    pid_t rg_pid = get_running_game_cached(rg_title, sizeof(rg_title), &rg_base);

    int n = snprintf(buf, cap,
        "{\"enabled\":%s,\"patches_last\":%d,\"patches_total\":%d,"
        "\"game_running\":%s,\"game_title_id\":\"%s\",\"game_pid\":%d}",
        atomic_load(&g_engine_enabled) ? "true" : "false",
        atomic_load(&g_patches_last),
        atomic_load(&g_patches_total),
        rg_pid > 0 ? "true" : "false",
        rg_title,
        (int)rg_pid);

    if (written) *written = (size_t)(n > 0 ? n : 0);
    return n > 0 ? 0 : -1;
}
