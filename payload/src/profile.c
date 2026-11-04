/* ps5upload — PS5 profile operations (avatar + offline account).
   See profile.h for the design + the GPL-3.0 offact attribution. */

#include "profile.h"
#include "atomic_file.h"
#include "sys_registry.h"

#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
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
    /* Heap, never the stack: this runs on a management thread, where a 64 KiB frame is
     * more than the stack budget (mgmt_audit.py) allows. */
    const size_t bufsz = 65536;
    char *buf = malloc(bufsz);
    if (!buf) {
        close(in);
        close(out);
        return -1;
    }
    int rc = 0;
    for (;;) {
        ssize_t n = read(in, buf, bufsz);
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
    free(buf);
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

/* ── PlayStation Network sign-in state: backup and restore ──────────────
 *
 * The Profile screen runs earthonion's np-fake-signin (fetched, not bundled). It copies a
 * template account's PSN state into the registry and writes NP files: auth.dat and
 * config.dat under /system_data/priv/home/<uid>/. Among the values the template carries are
 * NP_sub_account (2), NP_offl_acct_adult (0), NP_age (38) and a date of birth, and it writes
 * them to account slot 1's keys whatever slot the user is in. Afterwards some consoles start
 * asking to "Temporarily Disable PS5 Console Restrictions" before any game opens (a Discord
 * report, and upstream np-fake-signin issue #1), and Settings > Users and Accounts > Other >
 * Sign out, the undo its README gives, can be greyed out.
 *
 * So before it runs we keep, on the console, every value it overwrites and both files as they
 * were; restore puts them all back. The key list is exactly np-fake-signin v1.4's
 * write_registry_state(), at slot 1's offset and the user's slot's offset. Registry ids are
 * Sony's (names from ActRemoteLink's table). */

typedef struct {
    uint32_t base;  /* slot 1's key; slot n adds (n-1) * 65536 */
    uint16_t width; /* 0 = int, else the fixed string width np-fake-signin writes */
} np_key_t;

static const np_key_t NP_KEYS[] = {
    {125830656u, 65}, /* signin_id (e-mail) */
    {125874188u, 17}, /* NP_online_id */
    {125874183u, 17}, /* NP_env */
    {125874190u, 3},  /* NP_country_code */
    {125874191u, 6},  /* NP_language_code */
    {125874192u, 36}, /* NP_language_code_2 */
    {125874189u, 65}, /* NP_np_id */
    {125874193u, 11}, /* NP_date_of_birth */
    {125874195u, 65}, /* NP_m_account_id */
    {125830144u, 0},  /* auto_login */
    {125831424u, 0},  /* last_login_order */
    {125831168u, 0},  /* login_flag (offact's "flags") */
    {125832960u, 0},  /* user_status */
    {125874194u, 0},  /* NP_age */
    {125874185u, 0},  /* NP_sub_account */
    {125874186u, 0},  /* NP_offl_acct_adult */
    {125830912u, 0},  /* notification */
    {125831936u, 0},  /* friend_flag */
    {125832704u, 0},  /* created_version */
    {125882625u, 0},  /* (np-fake-signin's config.dat 0xD4) */
    {125854723u, 0},  /* TOPMENU_notificatn_flag */
    {125833216u, 0},  /* notifi_behavior */
};
#define NP_KEY_COUNT (sizeof(NP_KEYS) / sizeof(NP_KEYS[0]))
#define NP_STR_MAX   65
#define NP_FILE_MAX  (64 * 1024)

static uint32_t np_slot_off(int slot) {
    return (slot >= 1 && slot <= PROFILE_SLOT_COUNT) ? (uint32_t)(slot - 1) * 65536u : 0u;
}

#ifndef PROFILE_NP_HOME_ROOT
#define PROFILE_NP_HOME_ROOT "/system_data/priv/home"
#endif

static void np_file_path(char *out, size_t cap, uint32_t uid, int which) {
    if (which == 0)
        snprintf(out, cap, "%s/%x/np/auth.dat", PROFILE_NP_HOME_ROOT, uid);
    else
        snprintf(out, cap, "%s/%x/config.dat", PROFILE_NP_HOME_ROOT, uid);
}

static void np_backup_path(char *out, size_t cap, uint32_t uid) {
    snprintf(out, cap, "%s/0x%08X.txt", PROFILE_NP_BACKUP_ROOT, uid);
}

static void hex_into(FILE *f, const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n; i++) fprintf(f, "%02x", p[i]);
}

static int hex_nibble(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Decode up to `cap` bytes of hex; -1 on a bad digit or odd length. */
static long hex_decode(const char *s, uint8_t *out, size_t cap) {
    size_t n = 0;
    while (s[0] && s[0] != '\n' && s[0] != '\r') {
        int hi = hex_nibble(s[0]);
        int lo = s[1] ? hex_nibble(s[1]) : -1;
        if (hi < 0 || lo < 0 || n >= cap) return -1;
        out[n++] = (uint8_t)((hi << 4) | lo);
        s += 2;
    }
    return (long)n;
}

/* The whole file, or -1 when it is absent; -2 when it is unreadable or too big to keep. */
static long np_read_file(const char *path, uint8_t *buf, size_t cap) {
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return errno == ENOENT ? -1 : -2;
    long total = 0;
    for (;;) {
        ssize_t n = read(fd, buf + total, cap - (size_t)total);
        if (n < 0) {
            close(fd);
            return -2;
        }
        if (n == 0) break;
        total += n;
        if ((size_t)total == cap) {
            char extra;
            if (read(fd, &extra, 1) > 0) {
                close(fd);
                return -2;
            }
            break;
        }
    }
    close(fd);
    return total;
}

/* Write the registry rows for one offset: "i <key> <ok> <value>" / "s <key> <ok> <hex>". */
static void np_write_rows(FILE *f, uint32_t off) {
    for (size_t k = 0; k < NP_KEY_COUNT; k++) {
        uint32_t key = NP_KEYS[k].base + off;
        if (NP_KEYS[k].width == 0) {
            int v = 0;
            int ok = sys_registry_get_int(key, &v, NULL) == 0;
            fprintf(f, "i %u %d %d\n", key, ok, ok ? v : 0);
        } else {
            char buf[NP_STR_MAX + 1];
            memset(buf, 0, sizeof(buf));
            int ok = sys_registry_get_str(key, buf, NP_KEYS[k].width, NULL) == 0;
            fprintf(f, "s %u %d ", key, ok);
            hex_into(f, (const uint8_t *)buf, NP_KEYS[k].width);
            fputc('\n', f);
        }
    }
}

/* Adds `slot`'s rows to an existing backup unless it holds them already. The restore takes
 * each "slot N" line as the slot of the rows after it. Written whole to a temporary copy, on disk
 * before the rename, so a failed write or a crash leaves the old backup as it was. */
static int np_backup_add_slot(const char *path, int slot) {
    if (slot <= 1) return 0; /* slot 1's rows are always in the first section */
    FILE *in = fopen(path, "r");
    if (!in) return -1;
    char tmp[176];
    snprintf(tmp, sizeof(tmp), "%s.tmp", path);
    FILE *out = fopen(tmp, "w");
    if (!out) {
        fclose(in);
        return -1;
    }
    size_t cap = NP_FILE_MAX * 2 + 64;
    char *line = malloc(cap);
    int has = 0, rc = line ? 0 : -1;
    while (rc == 0 && fgets(line, (int)cap, in)) {
        int s = 0;
        if (sscanf(line, "slot %d", &s) == 1 && s == slot) has = 1;
        if (fputs(line, out) == EOF) rc = -1;
    }
    free(line);
    fclose(in);
    if (rc == 0 && !has) {
        fprintf(out, "slot %d\n", slot);
        np_write_rows(out, np_slot_off(slot));
    }
    if (atomic_file_close_stream(out) != 0) rc = -1;
    if (rc == 0 && !has && rename(tmp, path) != 0) rc = -1;
    if (rc != 0 || has) unlink(tmp);
    return rc;
}

int profile_np_backup(int slot, uint32_t uid, int *out_created) {
    if (out_created) *out_created = 0;
    if (slot < 1 || slot > PROFILE_SLOT_COUNT || uid == 0) return -1;
    char path[160];
    np_backup_path(path, sizeof(path), uid);
    struct stat st;
    /* The first backup is the state from before any sign-in: never replace it, or a second
     * run would keep the first run's changes as "the original". A sign-in from another slot
     * since then writes that slot's rows too, so they are added once, still untouched. */
    if (stat(path, &st) == 0) return np_backup_add_slot(path, slot);
    mkdir("/data/ps5upload", 0777);
    mkdir(PROFILE_NP_BACKUP_ROOT, 0777);
    char tmp[176];
    snprintf(tmp, sizeof(tmp), "%s.tmp", path);
    FILE *f = fopen(tmp, "w");
    if (!f) return -1;
    fprintf(f, "ps5upload-np-backup 1\nslot %d\nuid %u\n", slot, uid);
    np_write_rows(f, 0);
    if (slot > 1) np_write_rows(f, np_slot_off(slot));
    uint8_t *buf = malloc(NP_FILE_MAX);
    int rc = buf ? 0 : -1;
    for (int which = 0; rc == 0 && which < 2; which++) {
        char fp[96];
        np_file_path(fp, sizeof(fp), uid, which);
        long n = np_read_file(fp, buf, NP_FILE_MAX);
        if (n == -2) {
            rc = -1;
            break;
        }
        fprintf(f, "f %d %d ", which, n >= 0);
        if (n > 0) hex_into(f, buf, (size_t)n);
        fputc('\n', f);
    }
    free(buf);
    /* On disk before the rename: this is the one copy of the state from before any sign-in. */
    if (atomic_file_close_stream(f) != 0) rc = -1;
    if (rc == 0 && rename(tmp, path) != 0) rc = -1;
    if (rc != 0) {
        unlink(tmp);
        return -1;
    }
    if (out_created) *out_created = 1;
    return 0;
}

int profile_np_backup_exists(uint32_t uid, int *out_slot) {
    if (out_slot) *out_slot = 0;
    char path[160];
    np_backup_path(path, sizeof(path), uid);
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    char line[64];
    int slot = 0;
    while (fgets(line, sizeof(line), f)) {
        if (sscanf(line, "slot %d", &slot) == 1) break;
    }
    fclose(f);
    if (out_slot) *out_slot = slot;
    return 1;
}

/* Whether `key` is one np-fake-signin writes, at slot 1's offset or `slot`'s: the restore only
 * ever writes those, whatever the file says. */
static const np_key_t *np_key_for(uint32_t key, int slot) {
    for (size_t k = 0; k < NP_KEY_COUNT; k++) {
        if (key == NP_KEYS[k].base || (slot > 1 && key == NP_KEYS[k].base + np_slot_off(slot)))
            return &NP_KEYS[k];
    }
    return NULL;
}

static int np_put_file(const char *path, const uint8_t *data, size_t n) {
    char tmp[112];
    snprintf(tmp, sizeof(tmp), "%s.ps5upload-tmp", path);
    return atomic_file_write(path, tmp, data, n, 0644);
}

int profile_np_restore(uint32_t uid, int *out_written, int *out_failed) {
    if (out_written) *out_written = 0;
    if (out_failed) *out_failed = 0;
    char path[160];
    np_backup_path(path, sizeof(path), uid);
    FILE *f = fopen(path, "r");
    if (!f) return -2; /* no backup */
    /* A file row holds up to NP_FILE_MAX bytes as hex. */
    size_t cap = NP_FILE_MAX * 2 + 64;
    char *line = malloc(cap);
    uint8_t *bin = malloc(NP_FILE_MAX);
    if (!line || !bin) {
        free(line);
        free(bin);
        fclose(f);
        return -1;
    }
    int slot = 0, written = 0, failed = 0, header = 0, files_seen = 0;
    uint32_t file_uid = 0;
    while (fgets(line, (int)cap, f)) {
        /* Every row ends in a newline. Without one the file was cut here, and what is left of a number
         * or a hex row reads as a shorter value: stop, and keep the backup. */
        size_t line_len = strlen(line);
        if (line_len == 0 || line[line_len - 1] != '\n') {
            failed++;
            break;
        }
        if (strncmp(line, "ps5upload-np-backup 1", 21) == 0) {
            header = 1;
        } else if (sscanf(line, "slot %d", &slot) == 1) {
        } else if (sscanf(line, "uid %u", &file_uid) == 1) {
            if (file_uid != uid) break;
        } else if (header && slot >= 1 && file_uid == uid && (line[0] == 'i' || line[0] == 's')) {
            unsigned key = 0;
            int ok = 0, used = 0;
            if (sscanf(line + 2, "%u %d %n", &key, &ok, &used) < 2) continue;
            const np_key_t *nk = np_key_for((uint32_t)key, slot);
            if (!nk || (line[0] == 'i') != (nk->width == 0)) {
                failed++;
                continue;
            }
            /* Not readable before (some keys never are, e.g. auto_login on 13.60): there is no
             * value to put back, and writing one would invent it. */
            if (!ok) continue;
            int rc;
            if (line[0] == 'i') {
                int v = 0;
                if (sscanf(line + 2 + used, "%d", &v) != 1) {
                    failed++;
                    continue;
                }
                rc = sys_registry_set_int((uint32_t)key, v, NULL);
            } else {
                char s[NP_STR_MAX + 1];
                memset(s, 0, sizeof(s));
                /* The writer always keeps the whole field. */
                long n = hex_decode(line + 2 + used, (uint8_t *)s, nk->width);
                if (n != (long)nk->width) {
                    failed++;
                    continue;
                }
                rc = sys_registry_set_str((uint32_t)key, s, nk->width, NULL);
            }
            if (rc == 0)
                written++;
            else
                failed++;
        } else if (header && slot >= 1 && file_uid == uid && line[0] == 'f') {
            int which = -1, present = 0, used = 0;
            if (sscanf(line + 2, "%d %d %n", &which, &present, &used) < 2 || which < 0 || which > 1)
                continue;
            files_seen |= 1 << which;
            char fp[96];
            np_file_path(fp, sizeof(fp), uid, which);
            if (!present) {
                /* It was not there before: the sign-in made it. */
                if (unlink(fp) != 0 && errno != ENOENT) failed++;
                continue;
            }
            long n = hex_decode(line + 2 + used, bin, NP_FILE_MAX);
            if (n < 0 || np_put_file(fp, bin, (size_t)n) != 0)
                failed++;
            else
                written++;
        }
    }
    fclose(f);
    free(line);
    free(bin);
    if (!header || slot < 1 || file_uid != uid) return -1;
    if (files_seen != 3) failed++; /* A backup always holds both file rows: the rest of it is missing. */
    if (out_written) *out_written = written;
    if (out_failed) *out_failed = failed;
    if (failed == 0) {
        /* Kept, renamed, so a later sign-in takes a fresh backup of the restored state. */
        char done[176];
        snprintf(done, sizeof(done), "%s.restored", path);
        rename(path, done);
    }
    return failed == 0 ? 0 : -1;
}

/* A registry int for JSON: the number, or null when it could not be read. */
static void np_json_int(char *out, size_t cap, int ok, int v) {
    if (ok)
        snprintf(out, cap, "%d", v);
    else
        snprintf(out, cap, "null");
}

int profile_np_state_json(int slot, uint32_t uid, char *out, size_t cap) {
    if (!out || cap < 64) return -1;
    int bslot = 0;
    int have = profile_np_backup_exists(uid, &bslot);
    int sub = 0, adult = 0, age = 0, flags = 0;
    int sub_ok = sys_registry_get_int(125874185u + np_slot_off(slot), &sub, NULL) == 0;
    int adult_ok = sys_registry_get_int(125874186u + np_slot_off(slot), &adult, NULL) == 0;
    int age_ok = sys_registry_get_int(125874194u + np_slot_off(slot), &age, NULL) == 0;
    int flags_ok = sys_registry_get_int(125831168u + np_slot_off(slot), &flags, NULL) == 0;
    char online[18] = {0};
    sys_registry_get_str(125874188u + np_slot_off(slot), online, 17, NULL);
    /* The birth date np-fake-signin's template carries (1983-04-12) is how the app recognises
     * an account it signed in before backups existed. Digits and dashes only. */
    char dob[12] = {0};
    sys_registry_get_str(125874193u + np_slot_off(slot), dob, 11, NULL);
    for (size_t i = 0; dob[i]; i++) {
        if (!((dob[i] >= '0' && dob[i] <= '9') || dob[i] == '-')) {
            dob[0] = '\0';
            break;
        }
    }
    char oesc[40];
    size_t o = 0;
    for (size_t i = 0; online[i] && o + 7 < sizeof(oesc); i++) {
        unsigned char c = (unsigned char)online[i];
        if (c == '"' || c == '\\') {
            oesc[o++] = '\\';
            oesc[o++] = (char)c;
        } else if (c < 0x20 || c > 0x7e) {
            o += (size_t)snprintf(oesc + o, sizeof(oesc) - o, "\\u%04x", c);
        } else {
            oesc[o++] = (char)c;
        }
    }
    oesc[o] = '\0';
    char fp[96];
    struct stat st;
    np_file_path(fp, sizeof(fp), uid, 0);
    int auth = stat(fp, &st) == 0;
    np_file_path(fp, sizeof(fp), uid, 1);
    int config = stat(fp, &st) == 0;
    char vsub[16], vadult[16], vage[16], vflags[16];
    np_json_int(vsub, sizeof(vsub), sub_ok, sub);
    np_json_int(vadult, sizeof(vadult), adult_ok, adult);
    np_json_int(vage, sizeof(vage), age_ok, age);
    np_json_int(vflags, sizeof(vflags), flags_ok, flags);
    int n = snprintf(out, cap,
                     "{\"ok\":true,\"slot\":%d,\"uid\":%u,\"backup\":%s,\"backup_slot\":%d,"
                     "\"online_id\":\"%s\",\"dob\":\"%s\",\"auth_dat\":%s,\"config_dat\":%s,"
                     "\"sub_account\":%s,\"offline_adult\":%s,\"age\":%s,\"login_flag\":%s}",
                     slot, uid, have ? "true" : "false", bslot, oesc, dob, auth ? "true" : "false",
                     config ? "true" : "false", vsub, vadult, vage, vflags);
    return (n > 0 && (size_t)n < cap) ? n : -1;
}
