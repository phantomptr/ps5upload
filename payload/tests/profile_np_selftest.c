/* Host tests for the PSN sign-in backup (profile.c profile_np_*), real files in a fresh temp folder and
 * a table for the registry: what a sign-in changes comes back byte for byte, the first backup is never
 * replaced, nothing reaches the registry from a damaged file or outside the keys a sign-in writes, and
 * every replaced file is on disk before its rename. Build: see the root Makefile test-payload. */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ---- every fsync and rename, so the order can be checked ---- */

static int g_fsync_fail;
static int g_fsyncs, g_fsyncs_at_last_rename, g_unsynced_renames;

static int counted_fsync(int fd) {
    g_fsyncs++;
    if (g_fsync_fail) {
        errno = EIO;
        return -1;
    }
    return fsync(fd);
}

static int counted_rename(const char *from, const char *to) {
    /* ".restored" keeps bytes already on disk; everything else is a temp promoted over a real name. */
    size_t n = strlen(to);
    int keeps = n > 9 && strcmp(to + n - 9, ".restored") == 0;
    if (!keeps && g_fsyncs == g_fsyncs_at_last_rename) g_unsynced_renames++;
    g_fsyncs_at_last_rename = g_fsyncs;
    return rename(from, to);
}

static char g_backup_root[64], g_home_root[64];
#define PROFILE_NP_BACKUP_ROOT g_backup_root
#define PROFILE_NP_HOME_ROOT g_home_root

#define fsync counted_fsync
#define rename counted_rename
#include "../src/profile.c"
#undef fsync
#undef rename

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- the registry ---- */

#define REG_MAX 256
typedef struct {
    uint32_t key;
    int value;
    char text[NP_STR_MAX + 1];
} reg_row_t;

static reg_row_t g_reg[REG_MAX];
static int g_reg_count, g_writes, g_write_fail, g_foreign_writes;
static uint32_t g_unreadable;

/* A key's value until someone writes it: different per key, the same on every run. */
static reg_row_t *reg_row(uint32_t key) {
    for (int i = 0; i < g_reg_count; i++)
        if (g_reg[i].key == key) return &g_reg[i];
    if (g_reg_count == REG_MAX) abort();
    reg_row_t *r = &g_reg[g_reg_count++];
    memset(r, 0, sizeof(*r));
    r->key = key;
    r->value = (int)(key % 1000) + 1;
    snprintf(r->text, sizeof(r->text), "%u", key % 1000);
    return r;
}

static int reg_known(uint32_t key) {
    for (int slot = 1; slot <= PROFILE_SLOT_COUNT; slot++)
        if (np_key_for(key, slot)) return 1;
    return 0;
}

int sys_registry_get_int(uint32_t key, int *out, uint32_t *err) {
    (void)err;
    if (key == g_unreadable) return -1;
    *out = reg_row(key)->value;
    return 0;
}
int sys_registry_set_int(uint32_t key, int val, uint32_t *err) {
    (void)err;
    g_writes++;
    if (!reg_known(key)) g_foreign_writes++;
    if (g_write_fail) return -1;
    reg_row(key)->value = val;
    return 0;
}
int sys_registry_get_str(uint32_t key, char *buf, size_t cap, uint32_t *err) {
    (void)err;
    if (key == g_unreadable) return -1;
    snprintf(buf, cap, "%s", reg_row(key)->text);
    return 0;
}
int sys_registry_set_str(uint32_t key, const char *buf, size_t len, uint32_t *err) {
    (void)err;
    g_writes++;
    if (!reg_known(key)) g_foreign_writes++;
    if (g_write_fail) return -1;
    reg_row_t *r = reg_row(key);
    memset(r->text, 0, sizeof(r->text));
    memcpy(r->text, buf, len < NP_STR_MAX ? len : NP_STR_MAX);
    return 0;
}
int sys_registry_get_bin(uint32_t key, void *buf, size_t cap, uint32_t *err) {
    (void)key, (void)buf, (void)cap, (void)err;
    return -1;
}
int sys_registry_set_bin(uint32_t key, const void *buf, size_t cap, uint32_t *err) {
    (void)key, (void)buf, (void)cap, (void)err;
    return -1;
}

/* What a sign-in does to the registry, for every key of a slot. */
static void reg_scramble(int slot) {
    for (size_t k = 0; k < NP_KEY_COUNT; k++) {
        reg_row_t *r = reg_row(NP_KEYS[k].base + np_slot_off(slot));
        r->value = -7;
        snprintf(r->text, sizeof(r->text), "signed-in");
    }
}

/* A key's value as it was before any write. The narrowest field (3) only keeps the first two digits. */
static int reg_original(const np_key_t *nk, uint32_t key) {
    reg_row_t *r = reg_row(key);
    if (nk->width == 0) return r->value == (int)(key % 1000) + 1;
    char want[NP_STR_MAX + 1];
    snprintf(want, nk->width < sizeof(want) ? nk->width : sizeof(want), "%u", key % 1000);
    return strcmp(r->text, want) == 0;
}

static int reg_untouched(int slot) {
    for (size_t k = 0; k < NP_KEY_COUNT; k++)
        if (!reg_original(&NP_KEYS[k], NP_KEYS[k].base + np_slot_off(slot))) return 0;
    return 1;
}

/* ---- files ---- */

#define UID 0x1234abcdu

static void put_bytes(const char *path, const void *data, size_t n) {
    FILE *f = fopen(path, "wb");
    if (!f) abort();
    if (n && fwrite(data, 1, n, f) != n) abort();
    fclose(f);
}

static long file_size(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 ? (long)st.st_size : -1;
}

static int holds(const char *path, const void *data, size_t n) {
    if (file_size(path) != (long)n) return 0;
    char *buf = malloc(n + 1);
    FILE *f = fopen(path, "rb");
    if (!f || !buf) abort();
    size_t got = fread(buf, 1, n, f);
    fclose(f);
    int same = got == n && memcmp(buf, data, n) == 0;
    free(buf);
    return same;
}

static char *read_text(const char *path) {
    long n = file_size(path);
    if (n < 0) abort();
    char *buf = malloc((size_t)n + 1);
    FILE *f = fopen(path, "rb");
    if (!f || !buf) abort();
    size_t got = fread(buf, 1, (size_t)n, f);
    fclose(f);
    buf[got] = 0;
    return buf;
}

static char g_auth[96], g_config[96], g_backup[128], g_restored[144];
static uint8_t g_auth_bytes[1024];

/* A console as it was before any sign-in: auth.dat there (every byte value, newlines and NULs among
 * them), config.dat not, a fresh registry, no backup. */
static void fresh(void) {
    g_reg_count = 0;
    g_writes = g_write_fail = g_foreign_writes = 0;
    g_unreadable = 0;
    g_fsync_fail = 0;
    g_fsyncs = g_fsyncs_at_last_rename = g_unsynced_renames = 0;
    unlink(g_backup);
    unlink(g_restored);
    unlink(g_config);
    put_bytes(g_auth, g_auth_bytes, sizeof(g_auth_bytes));
}

/* What a sign-in does to the files. */
static void files_signed_in(void) {
    put_bytes(g_auth, "signed-in auth", 14);
    put_bytes(g_config, "signed-in config", 16);
}

static int no_temps(void) {
    char t[160];
    snprintf(t, sizeof(t), "%s.tmp", g_backup);
    if (file_size(t) >= 0) return 0;
    snprintf(t, sizeof(t), "%s.ps5upload-tmp", g_auth);
    if (file_size(t) >= 0) return 0;
    snprintf(t, sizeof(t), "%s.ps5upload-tmp", g_config);
    return file_size(t) < 0;
}

static int count_lines(const char *text, const char *line) {
    int n = 0;
    size_t len = strlen(line);
    for (const char *p = text; (p = strstr(p, line)) != NULL; p += len)
        if ((p == text || p[-1] == '\n') && p[len] == '\n') n++;
    return n;
}

/* ---- tests ---- */

static void test_round_trip(void) {
    fresh();
    int created = 0, written = 0, failed = 0, slot = 0;
    CHECK(profile_np_backup_exists(UID, &slot) == 0);
    CHECK(profile_np_restore(UID, &written, &failed) == -2);

    CHECK(profile_np_backup(1, UID, &created) == 0);
    CHECK(created == 1);
    CHECK(profile_np_backup_exists(UID, &slot) == 1 && slot == 1);

    reg_scramble(1);
    files_signed_in();
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(failed == 0 && written > 0);
    CHECK(reg_untouched(1));
    CHECK(holds(g_auth, g_auth_bytes, sizeof(g_auth_bytes)));
    CHECK(file_size(g_config) == -1); /* the sign-in made it, so it goes */
    CHECK(g_foreign_writes == 0);
    /* Kept under another name, so the next sign-in takes a fresh backup. */
    CHECK(profile_np_backup_exists(UID, &slot) == 0);
    CHECK(file_size(g_restored) > 0);
    CHECK(no_temps());
}

static void test_first_backup_stays(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0 && created == 1);
    reg_scramble(1);
    files_signed_in();
    /* A second run must not keep the first run's changes as "the original". */
    CHECK(profile_np_backup(1, UID, &created) == 0 && created == 0);
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(reg_untouched(1));
    CHECK(holds(g_auth, g_auth_bytes, sizeof(g_auth_bytes)));
}

static void test_other_slot_added_once(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    CHECK(profile_np_backup(3, UID, &created) == 0 && created == 0);
    CHECK(profile_np_backup(3, UID, &created) == 0);
    char *text = read_text(g_backup);
    CHECK(count_lines(text, "slot 1") == 1);
    CHECK(count_lines(text, "slot 3") == 1);
    free(text);
    CHECK(no_temps());
    reg_scramble(1);
    reg_scramble(3);
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(reg_untouched(1));
    CHECK(reg_untouched(3));
}

static void test_slot_above_one_keeps_slot_one_too(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(2, UID, &created) == 0 && created == 1);
    reg_scramble(1);
    reg_scramble(2);
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(reg_untouched(1));
    CHECK(reg_untouched(2));
}

static void test_unreadable_value_is_not_invented(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    g_unreadable = 125830144u; /* auto_login: not readable on 13.60 */
    CHECK(profile_np_backup(1, UID, &created) == 0);
    g_unreadable = 0;
    reg_row(125830144u)->value = 99;
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(reg_row(125830144u)->value == 99);
}

static void test_bad_arguments_write_nothing(void) {
    fresh();
    int created = 1;
    CHECK(profile_np_backup(0, UID, &created) == -1);
    CHECK(created == 0);
    CHECK(profile_np_backup(PROFILE_SLOT_COUNT + 1, UID, NULL) == -1);
    CHECK(profile_np_backup(1, 0, NULL) == -1);
    CHECK(file_size(g_backup) == -1);
}

/* Every temp reaches the disk before its rename: the first backup, a slot added to it, and the sign-in
 * files a restore puts back. */
static void test_bytes_are_on_disk_before_the_rename(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    CHECK(g_unsynced_renames == 0);
    CHECK(profile_np_backup(2, UID, &created) == 0);
    CHECK(g_unsynced_renames == 0);
    files_signed_in();
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(g_unsynced_renames == 0);
}

static void test_failed_sync_changes_nothing(void) {
    /* The first backup: no file at all, rather than an empty one that looks like the original state. */
    fresh();
    int created = 1, written = 0, failed = 0;
    g_fsync_fail = 1;
    CHECK(profile_np_backup(1, UID, &created) == -1);
    CHECK(created == 0);
    CHECK(file_size(g_backup) == -1);
    CHECK(no_temps());

    /* Adding a slot: the old backup stays as it was. */
    g_fsync_fail = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    char *before = read_text(g_backup);
    g_fsync_fail = 1;
    CHECK(profile_np_backup(2, UID, NULL) == -1);
    CHECK(holds(g_backup, before, strlen(before)));
    CHECK(no_temps());
    free(before);

    /* A restore that cannot write a file keeps the file it had, and the backup for another try. */
    files_signed_in();
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(failed > 0);
    CHECK(holds(g_auth, "signed-in auth", 14));
    CHECK(file_size(g_backup) > 0);
    CHECK(file_size(g_restored) == -1);
    CHECK(no_temps());
}

static void test_failed_registry_write_keeps_the_backup(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    g_write_fail = 1;
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(failed > 0);
    CHECK(file_size(g_backup) > 0);
    CHECK(file_size(g_restored) == -1);
}

/* Too big to keep whole: no backup, rather than one that cannot put it back. */
static void test_oversized_file_is_refused(void) {
    fresh();
    size_t big = NP_FILE_MAX + 1;
    char *data = calloc(1, big);
    if (!data) abort();
    put_bytes(g_auth, data, big);
    free(data);
    int created = 1;
    CHECK(profile_np_backup(1, UID, &created) == -1);
    CHECK(created == 0);
    CHECK(file_size(g_backup) == -1);
    CHECK(no_temps());

    /* Exactly the limit is kept, and comes back. */
    fresh();
    data = malloc(NP_FILE_MAX);
    if (!data) abort();
    memset(data, 0xab, NP_FILE_MAX);
    put_bytes(g_auth, data, NP_FILE_MAX);
    int written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0 && created == 1);
    put_bytes(g_auth, "x", 1);
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(holds(g_auth, data, NP_FILE_MAX));
    free(data);
}

/* The longest string a key takes. The getter's buffer holds the field with its NUL. */
static void test_longest_string_round_trips(void) {
    fresh();
    reg_row_t *r = reg_row(NP_KEYS[0].base);
    size_t longest = (size_t)NP_KEYS[0].width - 1;
    memset(r->text, 'a', longest);
    r->text[longest] = 0;
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    memset(r->text, 0, sizeof(r->text));
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
    CHECK(strlen(r->text) == longest);
    CHECK(strspn(r->text, "a") == longest);
}

static void write_backup_text(const char *text) { put_bytes(g_backup, text, strlen(text)); }

/* Cut at any point: each value is the signed-in one or its real one, never part of one, and only a
 * whole backup counts as restored. */
static void test_truncated_anywhere(void) {
    fresh();
    int created = 0, written = 0, failed = 0;
    CHECK(profile_np_backup(1, UID, &created) == 0);
    char *text = read_text(g_backup);
    size_t len = strlen(text);
    size_t step = len / 400 ? len / 400 : 1;
    for (size_t cut = 0; cut < len; cut += step) {
        g_reg_count = 0;
        reg_scramble(1);
        files_signed_in();
        unlink(g_restored);
        put_bytes(g_backup, text, cut);
        CHECK(profile_np_restore(UID, &written, &failed) == -1);
        CHECK(file_size(g_backup) == (long)cut); /* kept */
        CHECK(file_size(g_restored) == -1);
        for (size_t k = 0; k < NP_KEY_COUNT; k++) {
            uint32_t key = NP_KEYS[k].base;
            reg_row_t *row = reg_row(key);
            if (NP_KEYS[k].width == 0) {
                CHECK(row->value == -7 || reg_original(&NP_KEYS[k], key));
            } else {
                CHECK(strcmp(row->text, "signed-in") == 0 || reg_original(&NP_KEYS[k], key));
            }
        }
        CHECK(holds(g_auth, "signed-in auth", 14) || holds(g_auth, g_auth_bytes, sizeof(g_auth_bytes)));
    }
    free(text);
    /* The whole file, as a check that the loop above was not refusing everything. */
    fresh();
    CHECK(profile_np_backup(1, UID, &created) == 0);
    CHECK(profile_np_restore(UID, &written, &failed) == 0);
}

static void test_damaged_files_write_nothing(void) {
    int written = 0, failed = 0;
    char text[512];

    /* No header. */
    fresh();
    snprintf(text, sizeof(text), "slot 1\nuid %u\ni 125830144 1 5\n", UID);
    write_backup_text(text);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(g_writes == 0);

    /* No slot. */
    fresh();
    snprintf(text, sizeof(text), "ps5upload-np-backup 1\nuid %u\ni 125830144 1 5\n", UID);
    write_backup_text(text);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(g_writes == 0);

    /* Another user's backup. */
    fresh();
    snprintf(text, sizeof(text), "ps5upload-np-backup 1\nslot 1\nuid %u\ni 125830144 1 5\n", UID + 1);
    write_backup_text(text);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(g_writes == 0);

    /* Empty. */
    fresh();
    write_backup_text("");
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(g_writes == 0);

    /* A NUL where a row should start. */
    fresh();
    put_bytes(g_backup, "\0\n", 2);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(g_writes == 0);

    /* A key the sign-in does not write, an int row for a string key, a string longer than its field. */
    fresh();
    snprintf(text, sizeof(text),
             "ps5upload-np-backup 1\nslot 1\nuid %u\n"
             "i 1 1 5\n"
             "i 125830656 1 5\n"
             "s 125874190 1 6162636465666768\n",
             UID);
    write_backup_text(text);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(failed == 4); /* the three, and no file rows at all */
    CHECK(g_writes == 0);
    CHECK(file_size(g_backup) > 0);

    /* A file row that is not hex, odd in length, or for a file there is none of. */
    fresh();
    snprintf(text, sizeof(text),
             "ps5upload-np-backup 1\nslot 1\nuid %u\n"
             "f 0 1 zz\n"
             "f 1 1 abc\n"
             "f 9 1 ab\n",
             UID);
    write_backup_text(text);
    CHECK(profile_np_restore(UID, &written, &failed) == -1);
    CHECK(failed == 2);
    CHECK(holds(g_auth, g_auth_bytes, sizeof(g_auth_bytes)));
    CHECK(file_size(g_config) == -1);
}

int main(void) {
    char root[] = "/tmp/npXXXXXX";
    if (!mkdtemp(root)) return 1;
    snprintf(g_backup_root, sizeof(g_backup_root), "%s/b", root);
    snprintf(g_home_root, sizeof(g_home_root), "%s/h", root);
    char dir[96];
    if (mkdir(g_backup_root, 0700) != 0 || mkdir(g_home_root, 0700) != 0) return 1;
    snprintf(dir, sizeof(dir), "%s/%x", g_home_root, UID);
    if (mkdir(dir, 0700) != 0) return 1;
    snprintf(dir, sizeof(dir), "%s/%x/np", g_home_root, UID);
    if (mkdir(dir, 0700) != 0) return 1;
    snprintf(g_auth, sizeof(g_auth), "%s/%x/np/auth.dat", g_home_root, UID);
    snprintf(g_config, sizeof(g_config), "%s/%x/config.dat", g_home_root, UID);
    snprintf(g_backup, sizeof(g_backup), "%s/0x%08X.txt", g_backup_root, UID);
    snprintf(g_restored, sizeof(g_restored), "%s.restored", g_backup);
    for (size_t i = 0; i < sizeof(g_auth_bytes); i++) g_auth_bytes[i] = (uint8_t)i;

    test_round_trip();
    test_first_backup_stays();
    test_other_slot_added_once();
    test_slot_above_one_keeps_slot_one_too();
    test_unreadable_value_is_not_invented();
    test_bad_arguments_write_nothing();
    test_bytes_are_on_disk_before_the_rename();
    test_failed_sync_changes_nothing();
    test_failed_registry_write_keeps_the_backup();
    test_oversized_file_is_refused();
    test_longest_string_round_trips();
    test_truncated_anywhere();
    test_damaged_files_write_nothing();

    unlink(g_backup);
    unlink(g_restored);
    unlink(g_auth);
    unlink(g_config);
    snprintf(dir, sizeof(dir), "%s/%x/np", g_home_root, UID);
    rmdir(dir);
    snprintf(dir, sizeof(dir), "%s/%x", g_home_root, UID);
    rmdir(dir);
    rmdir(g_home_root);
    rmdir(g_backup_root);
    rmdir(root);
    if (failures) {
        fprintf(stderr, "profile_np_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("profile_np_selftest: ok\n");
    return 0;
}
