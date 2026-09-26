/* ps5upload — PS5 profile operations (avatar + offline account).
   See profile.h for the design + the GPL-3.0 offact attribution. */

#include "profile.h"
#include "sys_registry.h"

#include <dlfcn.h>
#include <dirent.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ── Offline-account registry key layout (vendored: ps5-payload-dev/offact,
 *    GPL-3.0). Each kind has a base key for slot 1 and a fixed stride
 *    between slots; `fallback` is used when the slot index is out of
 *    range. The numeric bases are Sony's registry IDs. ─────────────────── */
static uint32_t slot_key(int slot, uint32_t base, uint32_t stride,
                         uint32_t fallback) {
    if (slot < 1 || slot > PROFILE_SLOT_COUNT) return fallback;
    return (uint32_t)(slot - 1) * stride + base;
}

#define KEY_NAME(s)   slot_key((s), 125829632u, 65536u, 127140352u)
#define KEY_ID(s)     slot_key((s), 125830400u, 65536u, 127141120u)
#define KEY_TYPE(s)   slot_key((s), 125874183u, 65536u, 127184903u)
#define KEY_FLAGS(s)  slot_key((s), 125831168u, 65536u, 127141888u)

int profile_slot_get_name(int slot, char out[PROFILE_NAME_MAX],
                          uint32_t *out_err_code) {
    return sys_registry_get_str(KEY_NAME(slot), out, PROFILE_NAME_MAX,
                                out_err_code);
}

int profile_slot_set_name(int slot, const char *name, uint32_t *out_err_code) {
    if (!name) return -1;
    return sys_registry_set_str(KEY_NAME(slot), name, PROFILE_NAME_MAX,
                                out_err_code);
}

int profile_slot_get_id(int slot, uint64_t *out, uint32_t *out_err_code) {
    if (!out) return -1;
    *out = 0;
    return sys_registry_get_bin(KEY_ID(slot), out, sizeof(uint64_t),
                                out_err_code);
}

int profile_slot_set_id(int slot, uint64_t id, uint32_t *out_err_code) {
    return sys_registry_set_bin(KEY_ID(slot), &id, sizeof(uint64_t),
                                out_err_code);
}

int profile_slot_get_type(int slot, char out[PROFILE_TYPE_MAX],
                          uint32_t *out_err_code) {
    return sys_registry_get_str(KEY_TYPE(slot), out, PROFILE_TYPE_MAX,
                                out_err_code);
}

int profile_slot_set_type(int slot, const char *type, uint32_t *out_err_code) {
    if (!type) return -1;
    return sys_registry_set_str(KEY_TYPE(slot), type, PROFILE_TYPE_MAX,
                                out_err_code);
}

int profile_slot_get_flags(int slot, int *out, uint32_t *out_err_code) {
    return sys_registry_get_int(KEY_FLAGS(slot), out, out_err_code);
}

int profile_slot_set_flags(int slot, int flags, uint32_t *out_err_code) {
    return sys_registry_set_int(KEY_FLAGS(slot), flags, out_err_code);
}

uint64_t profile_gen_id(const char *name) {
    /* Verbatim from offact. The `0x5EAF00D / 0xCA7F00D` seed integer-
     * divides to 0 — an upstream quirk kept so derived ids match the
     * reference byte-for-byte. From there it's FNV-1a over the name. */
    uint64_t h = 0x5EAF00D / 0xCA7F00D;
    if (name && *name) {
        while (*name) {
            h = 0x100000001B3ULL * (h ^ (uint8_t)*name);
            name++;
        }
    }
    return h;
}

int profile_slot_activate(int slot, uint64_t id) {
    char name[PROFILE_NAME_MAX];
    if (profile_slot_get_name(slot, name, NULL) != 0 || !name[0]) return -1;
    if (!id) id = profile_gen_id(name);
    if (profile_slot_set_id(slot, id, NULL) != 0) return -1;
    if (profile_slot_set_type(slot, "np", NULL) != 0) return -1;
    if (profile_slot_set_flags(slot, PROFILE_DEFAULT_FLAGS, NULL) != 0)
        return -1;
    return 0;
}

int profile_slot_clear(int slot) {
    if (profile_slot_set_id(slot, 0, NULL) != 0) return -1;
    if (profile_slot_set_flags(slot, 0, NULL) != 0) return -1;
    return 0;
}

/* ── Foreground user (sceUserService) ───────────────────────────────────
 * Resolved by dlsym, same defensive pattern as sys_registry.c.
 * sceUserServiceInitialize is already called once at startup by
 * register.c, so we only need the getters here. */
typedef int (*uss_init_fn)(void *params);
typedef int (*uss_get_foreground_fn)(int *user_id);
typedef int (*uss_get_username_fn)(int32_t user_id, char *name, size_t size);
typedef int (*uss_set_username_fn)(int32_t user_id, const char *name);

static uss_init_fn           g_uss_init = NULL;
static uss_get_foreground_fn g_uss_get_fg = NULL;
static uss_get_username_fn   g_uss_get_name = NULL;
static uss_set_username_fn   g_uss_set_name = NULL;
static pthread_once_t        g_uss_once = PTHREAD_ONCE_INIT;

static void uss_resolve_impl(void) {
    g_uss_init = (uss_init_fn)dlsym(RTLD_DEFAULT, "sceUserServiceInitialize");
    g_uss_get_fg = (uss_get_foreground_fn)dlsym(
        RTLD_DEFAULT, "sceUserServiceGetForegroundUser");
    g_uss_get_name = (uss_get_username_fn)dlsym(
        RTLD_DEFAULT, "sceUserServiceGetUserName");
    g_uss_set_name = (uss_set_username_fn)dlsym(
        RTLD_DEFAULT, "sceUserServiceSetUserName");
    /* The name getters/setters need the service initialized; idempotent. */
    if (g_uss_init) g_uss_init(NULL);
}

int profile_set_local_username(uint32_t uid, const char *name) {
    if (!name) return -1;
    pthread_once(&g_uss_once, uss_resolve_impl);
    if (!g_uss_set_name) return -1;
    return g_uss_set_name((int32_t)uid, name) == 0 ? 0 : -1;
}

int profile_user_name(uint32_t uid, char *name_out, size_t name_out_size) {
    if (name_out && name_out_size > 0) name_out[0] = '\0';
    if (!name_out || name_out_size == 0) return -1;
    pthread_once(&g_uss_once, uss_resolve_impl);
    if (!g_uss_get_name) return -1;
    char tmp[17] = {0};
    if (g_uss_get_name((int32_t)uid, tmp, sizeof(tmp)) != 0) return -1;
    size_t n = strlen(tmp);
    if (n >= name_out_size) n = name_out_size - 1;
    memcpy(name_out, tmp, n);
    name_out[n] = '\0';
    return 0;
}

uint32_t profile_foreground_user(char *name_out, size_t name_out_size) {
    if (name_out && name_out_size > 0) name_out[0] = '\0';
    pthread_once(&g_uss_once, uss_resolve_impl);
    if (!g_uss_get_fg) return 0;
    int uid = 0;
    /* uid <= 0 covers both "no foreground" (0) and Sony's -1 sentinel. */
    if (g_uss_get_fg(&uid) != 0 || uid <= 0) return 0;
    if (name_out && name_out_size > 0) {
        profile_user_name((uint32_t)uid, name_out, name_out_size);
    }
    return (uint32_t)uid;
}

/* ── Avatar apply (privileged staging → profile-cache copy) ─────────────── */

/* mkdir ignoring "already exists" — the profile-cache ancestors almost
 * always exist on the console; we create the per-UID leaf. */
static void ensure_dir(const char *path) {
    if (mkdir(path, 0755) != 0) {
        /* EEXIST and friends are fine; a real failure surfaces later when
         * the copy can't open its destination. */
    }
}

static int copy_file(const char *src, const char *dst) {
    int in = open(src, O_RDONLY | O_NOFOLLOW);
    if (in < 0) return -1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0644);
    if (out < 0) {
        close(in);
        return -1;
    }
    char buf[65536];
    int rc = 0;
    for (;;) {
        ssize_t n = read(in, buf, sizeof(buf));
        if (n < 0) {
            rc = -1;
            break;
        }
        if (n == 0) break;
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out, buf + off, (size_t)(n - off));
            if (w < 0) {
                rc = -1;
                break;
            }
            off += w;
        }
        if (rc != 0) break;
    }
    close(in);
    close(out);
    return rc;
}

int profile_apply_avatar(uint32_t uid, int *out_copied) {
    if (out_copied) *out_copied = 0;

    char stage[256];
    char dest[256];
    snprintf(stage, sizeof(stage), "%s/0x%08X", PROFILE_STAGE_ROOT, uid);
    snprintf(dest, sizeof(dest),
             "/system_data/priv/cache/profile/0x%08X", uid);

    DIR *d = opendir(stage);
    if (!d) return -1;

    /* Create the destination tree (ancestors usually pre-exist). */
    ensure_dir("/system_data");
    ensure_dir("/system_data/priv");
    ensure_dir("/system_data/priv/cache");
    ensure_dir("/system_data/priv/cache/profile");
    ensure_dir(dest);

    int copied = 0;
    int rc = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.') continue; /* skip ., .., dotfiles */
        char src[512];
        char dst[512];
        snprintf(src, sizeof(src), "%s/%s", stage, e->d_name);
        struct stat st;
        if (stat(src, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        snprintf(dst, sizeof(dst), "%s/%s", dest, e->d_name);
        if (copy_file(src, dst) == 0) {
            copied++;
        } else {
            rc = -1;
        }
    }
    closedir(d);

    if (out_copied) *out_copied = copied;
    if (copied == 0) return -1; /* nothing applied is a failure */
    return rc;
}
