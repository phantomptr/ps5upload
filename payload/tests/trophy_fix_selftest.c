/* Host self-test of the trophy account fix (trophy_fix.c, #853).
 *
 *   cc -Ipayload/include payload/tests/trophy_fix_selftest.c payload/src/trophy_fix.c \
 *      payload/src/app_dump_sha256.c
 *
 * Builds TRPTITLE.DAT files the way the console lays them out (T2PD: header, 0x400-byte blocks each
 * with its SHA-256) and checks: only the exact old id is replaced, in both byte orders, a match across
 * two blocks included; every hash checks out afterwards; an old id of 0 only ever changes the account
 * field; a file of another user or with a bad hash is left alone; in a folder tree, links are never
 * followed, a dry run changes nothing and makes no backup, a run keeps a backup outside the trophy
 * folder first, and the backup puts the file back. */
#include "trophy_fix.h"
#include "app_dump_sha256.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;
#define CHECK(c)                                                                                   \
    do {                                                                                           \
        if (!(c)) {                                                                                \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c);                           \
            failures++;                                                                            \
        }                                                                                          \
    } while (0)

#define UID 0x1ea2f4dau
#define OLD 0x1122334455667788ull
#define NEW 0x99aabbccddeeff01ull
#define THIRD 0x0badc0ffee123456ull

static void be64_at(uint8_t *p, uint64_t v) {
    for (int i = 7; i >= 0; i--, v >>= 8) p[i] = (uint8_t)v;
}
static void le64_at(uint8_t *p, uint64_t v) {
    for (int i = 0; i < 8; i++, v >>= 8) p[i] = (uint8_t)v;
}
static uint64_t rd_be64(const uint8_t *p) {
    uint64_t v = 0;
    for (int i = 0; i < 8; i++) v = (v << 8) | p[i];
    return v;
}

/* data offset -> file offset */
static size_t at(size_t d) { return T2PD_HEAD + (d / T2PD_CHUNK) * T2PD_STRIDE + d % T2PD_CHUNK; }

static void rehash(uint8_t *f, size_t blocks) {
    for (size_t b = 0; b < blocks; b++) {
        uint8_t *c = f + T2PD_HEAD + b * T2PD_STRIDE;
        int zero = 1;
        for (size_t i = 0; i < T2PD_CHUNK; i++)
            if (c[i]) zero = 0;
        if (zero) {
            memset(c + T2PD_CHUNK, 0, 32);
            continue;
        }
        SHA256_CTX x;
        sha256_init(&x);
        sha256_update(&x, c, T2PD_CHUNK);
        sha256_final(&x, c + T2PD_CHUNK);
    }
}

/* A TRPTITLE.DAT of `blocks` blocks naming `account` and `uid`; block 3 is left all zero. */
static uint8_t *make_dat(size_t blocks, uint64_t account, uint32_t uid, size_t *n) {
    *n = T2PD_HEAD + blocks * T2PD_STRIDE;
    uint8_t *f = calloc(1, *n);
    memcpy(f, "T2PD", 4);
    f[5] = 1; /* 0x10000 */
    be64_at(f + 8, T2PD_STRIDE);
    be64_at(f + 0x20, blocks);
    memcpy(f + at(0), "T2TD", 4);
    for (size_t d = 0x10; d < blocks * T2PD_CHUNK; d++)
        if (d / T2PD_CHUNK != 3) f[at(d)] = (uint8_t)(d * 7 + 3);
    /* a run of zeros in the data, where an old id of 0 must not be matched */
    memset(f + at(0x300), 0, 32);
    be64_at(f + T2PD_ACCOUNT_AT, account);
    f[T2PD_USER_AT] = (uint8_t)(uid >> 24);
    f[T2PD_USER_AT + 1] = (uint8_t)(uid >> 16);
    f[T2PD_USER_AT + 2] = (uint8_t)(uid >> 8);
    f[T2PD_USER_AT + 3] = (uint8_t)uid;
    rehash(f, blocks);
    return f;
}

static void test_memory(void) {
    size_t n;
    trophy_t2pd_result_t r;

    /* the account field only: two places change, the field and block 0's hash */
    uint8_t *f = make_dat(5, OLD, UID, &n);
    uint8_t *orig = malloc(n);
    memcpy(orig, f, n);
    CHECK(trophy_t2pd_valid(f, n));
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 0, &r) == TF_CHANGE && r.anchor && r.extra == 0);
    CHECK(memcmp(f, orig, n) == 0); /* a dry look changes nothing */
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 1, &r) == TF_CHANGE && r.blocks == 1);
    CHECK(rd_be64(f + T2PD_ACCOUNT_AT) == NEW && r.from == OLD);
    CHECK(trophy_t2pd_valid(f, n));
    for (size_t i = 0; i < n; i++)
        if (f[i] != orig[i] && !(i >= 0x200 && i < 0x208) && !(i >= 0x440 && i < 0x460)) {
            CHECK(!"changed outside the account field and its block's hash");
            break;
        }
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 0, &r) == TF_ALREADY);
    free(f);

    /* the old id elsewhere: big endian across blocks 0/1, little endian in block 2; near misses kept */
    f = make_dat(5, OLD, UID, &n);
    uint8_t be[8], le[8], miss[8];
    be64_at(be, OLD);
    le64_at(le, OLD);
    memcpy(miss, be, 8);
    miss[7] ^= 1;
    for (int i = 0; i < 8; i++) f[at(0x3fc + i)] = be[i];
    memcpy(f + at(0x900), le, 8);
    memcpy(f + at(0x1100), miss, 8);
    rehash(f, 5);
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 1, &r) == TF_CHANGE && r.anchor && r.extra == 2);
    CHECK(r.blocks == 3 && trophy_t2pd_valid(f, n));
    uint8_t nb[8], nl[8], got[8];
    be64_at(nb, NEW);
    le64_at(nl, NEW);
    for (int i = 0; i < 8; i++) got[i] = f[at(0x3fc + i)];
    CHECK(memcmp(got, nb, 8) == 0);
    CHECK(memcmp(f + at(0x900), nl, 8) == 0);
    CHECK(memcmp(f + at(0x1100), miss, 8) == 0);
    free(f);

    /* old id 0: the account field only, never the zeros in the data */
    f = make_dat(5, 0, UID, &n);
    memcpy(orig = realloc(orig, n), f, n);
    CHECK(trophy_t2pd_fix(f, n, UID, 0, 1, NEW, 1, &r) == TF_CHANGE && r.anchor && r.extra == 0);
    CHECK(memcmp(f + at(0x300), orig + at(0x300), 32) == 0 && trophy_t2pd_valid(f, n));
    free(f);

    /* a third id: left alone with the old id given, changed without one */
    f = make_dat(5, THIRD, UID, &n);
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 1, &r) == TF_OTHER_ID && r.from == THIRD);
    CHECK(rd_be64(f + T2PD_ACCOUNT_AT) == THIRD);
    CHECK(trophy_t2pd_fix(f, n, UID, 0, 0, NEW, 1, &r) == TF_CHANGE && rd_be64(f + T2PD_ACCOUNT_AT) == NEW);
    free(f);

    /* another user's file, and one whose hashes are wrong */
    f = make_dat(5, OLD, UID + 1, &n);
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 1, &r) == TF_OTHER_USER);
    CHECK(rd_be64(f + T2PD_ACCOUNT_AT) == OLD);
    free(f);
    f = make_dat(5, OLD, UID, &n);
    f[at(0x500)] ^= 0xff;
    memcpy(orig = realloc(orig, n), f, n);
    CHECK(trophy_t2pd_fix(f, n, UID, OLD, 1, NEW, 1, &r) == TF_BAD_HASH && memcmp(f, orig, n) == 0);
    free(f);

    /* not a T2PD: reported when it holds the old id, never when the old id is 0 */
    uint8_t other[64] = {0};
    memcpy(other + 20, le, 8);
    CHECK(trophy_t2pd_fix(other, sizeof other, UID, OLD, 1, NEW, 1, &r) == TF_UNKNOWN && r.extra == 1);
    CHECK(memcmp(other + 20, le, 8) == 0);
    CHECK(trophy_t2pd_fix(other, sizeof other, UID, 0, 1, NEW, 1, &r) == TF_NONE);
    free(orig);
}

static void put(const char *path, const uint8_t *p, size_t n) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0640);
    if (fd >= 0) {
        CHECK(write(fd, p, n) == (ssize_t)n);
        close(fd);
    }
}

static uint8_t *get(const char *path, size_t *n) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    uint8_t *b = malloc(TROPHY_FIX_FILE_MAX);
    *n = fread(b, 1, TROPHY_FIX_FILE_MAX, f);
    fclose(f);
    return b;
}

static int exists(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0;
}

static void test_tree(void) {
    char base[] = "/tmp/trophy-fix-selftest-XXXXXX";
    if (!mkdtemp(base)) {
        CHECK(!"mkdtemp");
        return;
    }
    char home[256], bk[256], p[512], q[512], data[512], outside[256];
    snprintf(home, sizeof home, "%s/home", base);
    snprintf(bk, sizeof bk, "%s/backup", base);
    snprintf(outside, sizeof outside, "%s/outside", base);
    mkdir(home, 0755);
    mkdir(outside, 0755);
    snprintf(p, sizeof p, "%s/%08x", home, UID);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/%08x/trophy2", home, UID);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/%08x/trophy2/nobackup", home, UID);
    mkdir(p, 0755);
    snprintf(data, sizeof data, "%s/%08x/trophy2/nobackup/data", home, UID);
    mkdir(data, 0755);

    size_t n, m;
    uint8_t *f1 = make_dat(4, OLD, UID, &n);
    uint8_t *f2 = make_dat(4, NEW, UID, &n);
    snprintf(p, sizeof p, "%s/NPWR00001_00", data);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/NPWR00001_00/TRPTITLE.DAT", data);
    put(p, f1, n);
    snprintf(p, sizeof p, "%s/NPWR00002_00", data);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/NPWR00002_00/TRPTITLE.DAT", data);
    put(p, f2, n);
    /* a file of unknown layout holding the old id: reported, not changed */
    uint8_t blob[64] = {0};
    be64_at(blob + 9, OLD);
    snprintf(p, sizeof p, "%s/NPWR00002_00/OTHER.DAT", data);
    put(p, blob, sizeof blob);
    /* links: a folder and a file that point out of the trophy folder, never followed */
    snprintf(q, sizeof q, "%s/TRPTITLE.DAT", outside);
    put(q, f1, n);
    snprintf(p, sizeof p, "%s/NPWR00003_00", data);
    CHECK(symlink(outside, p) == 0);
    snprintf(p, sizeof p, "%s/NPWR00004_00", data);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/NPWR00004_00/TRPTITLE.DAT", data);
    CHECK(symlink(q, p) == 0);

    trophy_fix_roots_t roots = {home, bk};
    char *out = malloc(65536);
    char bkdir[600];
    snprintf(bkdir, sizeof bkdir, "%s/%08x", bk, UID);

    /* dry run: nothing changes, no backup is made */
    int len = trophy_fix_run(&roots, UID, OLD, 1, NEW, 1, "20261117-120000", out, 65536);
    CHECK(len > 0);
    CHECK(strstr(out, "\"ok\":true") && strstr(out, "\"changed\":1") && strstr(out, "\"already\":1"));
    CHECK(strstr(out, "would_change") && strstr(out, "unknown_format"));
    CHECK(strstr(out, "trophy2/nobackup/data/NPWR00001_00/TRPTITLE.DAT"));
    CHECK(!strstr(out, "NPWR00003_00") && !strstr(out, "NPWR00004_00"));
    CHECK(!exists(bk));
    snprintf(p, sizeof p, "%s/NPWR00001_00/TRPTITLE.DAT", data);
    uint8_t *now = get(p, &m);
    CHECK(now && m == n && memcmp(now, f1, n) == 0);
    free(now);

    /* the run: backup first, then the file */
    len = trophy_fix_run(&roots, UID, OLD, 1, NEW, 0, "20261117-120000", out, 65536);
    CHECK(len > 0 && strstr(out, "\"changed\":1") && strstr(out, "\"failed\":0"));
    CHECK(strstr(out, "\"backup\":\"20261117-120000\""));
    now = get(p, &m);
    CHECK(now && m == n && rd_be64(now + T2PD_ACCOUNT_AT) == NEW && trophy_t2pd_valid(now, m));
    free(now);
    struct stat st;
    CHECK(lstat(p, &st) == 0 && (st.st_mode & 0777) == 0640);
    snprintf(q, sizeof q, "%s/20261117-120000/trophy2/nobackup/data/NPWR00001_00/TRPTITLE.DAT", bkdir);
    now = get(q, &m);
    CHECK(now && m == n && memcmp(now, f1, n) == 0);
    free(now);
    snprintf(q, sizeof q, "%s/TRPTITLE.DAT", outside);
    now = get(q, &m);
    CHECK(now && memcmp(now, f1, n) == 0); /* behind the links: untouched */
    free(now);
    snprintf(q, sizeof q, "%s/NPWR00002_00/OTHER.DAT", data);
    now = get(q, &m);
    CHECK(now && m == sizeof blob && memcmp(now, blob, m) == 0);
    free(now);
    snprintf(q, sizeof q, "%s/NPWR00001_00/TRPTITLE.DAT.ps5upload-tmp", data);
    CHECK(!exists(q));

    /* a second run finds nothing to do and makes no second backup */
    len = trophy_fix_run(&roots, UID, OLD, 1, NEW, 0, "20261117-120001", out, 65536);
    CHECK(len > 0 && strstr(out, "\"changed\":0") && strstr(out, "\"backup\":\"\""));
    snprintf(q, sizeof q, "%s/20261117-120001", bkdir);
    CHECK(!exists(q));

    /* the backups, and putting one back */
    len = trophy_fix_backups_json(&roots, UID, out, 65536);
    CHECK(len > 0 && strstr(out, "{\"name\":\"20261117-120000\",\"files\":1}"));
    len = trophy_fix_restore(&roots, UID, "20261117-120000", out, 65536);
    CHECK(len > 0 && strstr(out, "\"ok\":true") && strstr(out, "\"restored\":1"));
    now = get(p, &m);
    CHECK(now && m == n && memcmp(now, f1, n) == 0);
    free(now);
    CHECK(trophy_fix_restore(&roots, UID, "../x", out, 65536) > 0 && strstr(out, "bad_request"));
    CHECK(trophy_fix_restore(&roots, UID, "20000101-000000", out, 65536) > 0 && strstr(out, "no_such_backup"));

    /* no old id (the Repair action): the account field is set whatever it holds */
    uint8_t *f3 = make_dat(4, THIRD, UID, &n);
    put(p, f3, n);
    len = trophy_fix_run(&roots, UID, 0, 0, NEW, 0, "20261117-120002", out, 65536);
    CHECK(len > 0 && strstr(out, "\"old\":null") && strstr(out, "\"changed\":1"));
    now = get(p, &m);
    CHECK(now && rd_be64(now + T2PD_ACCOUNT_AT) == NEW);
    free(now);

    /* bad requests */
    CHECK(trophy_fix_run(&roots, UID, OLD, 1, 0, 1, "1", out, 65536) > 0 && strstr(out, "bad_request"));
    CHECK(trophy_fix_run(&roots, UID + 5, OLD, 1, NEW, 1, "1", out, 65536) > 0 && strstr(out, "no_such_user"));
    CHECK(trophy_fix_run(&roots, UID, OLD, 1, NEW, 1, "../x", out, 65536) > 0 && strstr(out, "bad_request"));

    free(out);
    free(f1);
    free(f2);
    free(f3);
    char cmd[400];
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", base);
    if (system(cmd) != 0) fprintf(stderr, "could not remove %s\n", base);
}

int main(void) {
    test_memory();
    test_tree();
    if (failures) {
        fprintf(stderr, "trophy_fix_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("trophy_fix_selftest: ok\n");
    return 0;
}
