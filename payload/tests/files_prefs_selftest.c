/* Host-side tests for the file manager's shared prefs (files_prefs.c): path cleaning, bounds
 * (favourites, recent, path length, the file's 64 KiB cap), garbage and cut-short files reading as
 * empty and being replaced by the next save, de-duplication, recent ordering, the atomic same-
 * directory save (no temp file left, a failed save keeps the old file) and concurrent writers
 * (every answer and the final file parse). File tests run in a fresh mkdtemp directory.
 * Build: see the root Makefile test-payload. */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "files_prefs.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static char g_dir[256], g_file[300];
static char g_out[FILES_PREFS_FILE_MAX + 1];

static void put_file(const char *path, const char *text, size_t n) {
    FILE *f = fopen(path, "wb");
    if (!f) {
        perror(path);
        exit(2);
    }
    fwrite(text, 1, n, f);
    fclose(f);
}

static int exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static files_prefs_t *fresh(void) {
    files_prefs_t *p = malloc(sizeof *p);
    if (!p) exit(2);
    files_prefs_clear(p);
    return p;
}

static void test_clean_path(void) {
    char out[FILES_PREFS_PATH_MAX], big[FILES_PREFS_PATH_MAX + 8];
    CHECK(files_prefs_clean_path("/data/games", out) == 0 && strcmp(out, "/data/games") == 0);
    CHECK(files_prefs_clean_path("/data//games/", out) == 0 && strcmp(out, "/data/games") == 0);
    CHECK(files_prefs_clean_path("/", out) == 0 && strcmp(out, "/") == 0);
    CHECK(files_prefs_clean_path("//", out) == 0 && strcmp(out, "/") == 0);
    CHECK(files_prefs_clean_path("/a/.../b", out) == 0); /* "..." is a name */
    /* UTF-8 names are kept; bytes that are not UTF-8 would not read back as themselves */
    CHECK(files_prefs_clean_path("/data/\xE3\x82\xB2\xE3\x83\xBC\xE3\x83\xA0 \xF0\x9F\x8E\xAE", out) == 0);
    CHECK(files_prefs_clean_path("/data/Caf\xE9", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/data/\xFF\xFF", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/data/\xE3\x82", out) == FILES_PREFS_E_BAD_PATH); /* cut short */
    CHECK(files_prefs_clean_path("/data/\xED\xA0\x80", out) == FILES_PREFS_E_BAD_PATH); /* surrogate */
    CHECK(files_prefs_clean_path("/data/\xC0\xAF", out) == FILES_PREFS_E_BAD_PATH); /* overlong '/' */
    CHECK(files_prefs_clean_path("/data/My..Game", out) == 0);
    CHECK(files_prefs_clean_path("data/games", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/data/../etc", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/data/./x", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/data/..", out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_clean_path("/a\nb", out) == FILES_PREFS_E_BAD_PATH);
    /* 1023 bytes fit (with the NUL: FILES_PREFS_PATH_MAX), 1024 do not */
    memset(big, 'a', sizeof big);
    big[0] = '/';
    big[FILES_PREFS_PATH_MAX - 1] = '\0';
    CHECK(files_prefs_clean_path(big, out) == 0 && strlen(out) == FILES_PREFS_PATH_MAX - 1);
    big[FILES_PREFS_PATH_MAX - 1] = 'a';
    big[FILES_PREFS_PATH_MAX] = '\0';
    CHECK(files_prefs_clean_path(big, out) == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_op_from_name("pin") == FILES_PREFS_PIN && files_prefs_op_from_name("unpin") == FILES_PREFS_UNPIN &&
          files_prefs_op_from_name("visit") == FILES_PREFS_VISIT && files_prefs_op_from_name("forget") == FILES_PREFS_FORGET &&
          files_prefs_op_from_name("delete") == 0 &&
          files_prefs_op_from_name("") == 0);
}

static void test_bounds(void) {
    files_prefs_t *p = fresh();
    char path[64];
    int i;
    for (i = 0; i < FILES_PREFS_FAV_MAX; i++) {
        snprintf(path, sizeof path, "/data/f%d", i);
        CHECK(files_prefs_pin(p, path) == 0);
    }
    CHECK(p->nfav == FILES_PREFS_FAV_MAX);
    CHECK(files_prefs_pin(p, "/data/one-more") == FILES_PREFS_E_FULL);
    CHECK(files_prefs_pin(p, "/data/f3") == 0 && p->nfav == FILES_PREFS_FAV_MAX); /* already pinned: no change */
    CHECK(files_prefs_unpin(p, "/data/f0/") == 0 && p->nfav == FILES_PREFS_FAV_MAX - 1 && strcmp(p->fav[0], "/data/f1") == 0);
    CHECK(files_prefs_unpin(p, "/data/never") == 0 && p->nfav == FILES_PREFS_FAV_MAX - 1);
    CHECK(files_prefs_pin(p, "relative") == FILES_PREFS_E_BAD_PATH);
    CHECK(files_prefs_visit(p, "/x/../y") == FILES_PREFS_E_BAD_PATH && !p->has_last);
    for (i = 0; i < 30; i++) {
        snprintf(path, sizeof path, "/mnt/usb0/r%d", i);
        CHECK(files_prefs_visit(p, path) == 0);
    }
    CHECK(p->nrecent == FILES_PREFS_RECENT_MAX);
    CHECK(strcmp(p->recent[0], "/mnt/usb0/r29") == 0 && strcmp(p->recent[FILES_PREFS_RECENT_MAX - 1], "/mnt/usb0/r18") == 0);
    CHECK(p->has_last && strcmp(p->last, "/mnt/usb0/r29") == 0);
    free(p);
}

static void test_recent_order(void) {
    files_prefs_t *p = fresh();
    files_prefs_visit(p, "/data");
    files_prefs_visit(p, "/data/homebrew");       /* walking down: replaces /data */
    files_prefs_visit(p, "/data/homebrew/game1"); /* and again */
    CHECK(p->nrecent == 1 && strcmp(p->recent[0], "/data/homebrew/game1") == 0);
    files_prefs_visit(p, "/mnt/usb0");
    files_prefs_visit(p, "/data/homebrew"); /* back up: an ancestor of an older entry stays apart */
    CHECK(p->nrecent == 3);
    CHECK(strcmp(p->recent[0], "/data/homebrew") == 0 && strcmp(p->recent[1], "/mnt/usb0") == 0 &&
          strcmp(p->recent[2], "/data/homebrew/game1") == 0);
    files_prefs_visit(p, "/mnt/usb0/"); /* revisit: moves to the front, once */
    CHECK(p->nrecent == 3 && strcmp(p->recent[0], "/mnt/usb0") == 0 && strcmp(p->recent[1], "/data/homebrew") == 0);
    files_prefs_visit(p, "/mnt/usb0b"); /* a sibling with the same prefix is not a child */
    CHECK(p->nrecent == 4 && strcmp(p->recent[1], "/mnt/usb0") == 0);
    CHECK(strcmp(p->last, "/mnt/usb0b") == 0);
    /* forget: a gone folder leaves recent (and last, when it is that one); others keep their order */
    CHECK(files_prefs_forget(p, "/mnt/usb0/") == 0 && p->nrecent == 3 && strcmp(p->recent[0], "/mnt/usb0b") == 0 &&
          strcmp(p->recent[1], "/data/homebrew") == 0 && p->has_last);
    CHECK(files_prefs_forget(p, "/mnt/usb0b") == 0 && p->nrecent == 2 && !p->has_last);
    CHECK(files_prefs_forget(p, "/never") == 0 && p->nrecent == 2);
    CHECK(files_prefs_forget(p, "rel") == FILES_PREFS_E_BAD_PATH);
    free(p);
}

static void test_parse_and_dedup(void) {
    files_prefs_t *p = fresh();
    const char *ok = "  {\"v\":1,\"favorites\":[\"/data/a\",\"/data/a/\",\"/data//b\",\"rel\",\"/x/../y\",\"/data/b\"],"
                     "\"recent\":[\"/r1\",\"/r1\",\"/r2\"],\"last\":\"/data/a\",\"extra\":{\"k\":[1,2]}}\n";
    const char *cjk = "{\"favorites\":[\"/data/\\u5b8c\\u86cb \\\"q\\\" \\\\b\"],\"recent\":[],\"last\":null}";
    char json[512];
    files_prefs_t *q;
    int len;
    CHECK(files_prefs_parse(ok, strlen(ok), p) == 0);
    CHECK(p->nfav == 2 && strcmp(p->fav[0], "/data/a") == 0 && strcmp(p->fav[1], "/data/b") == 0);
    CHECK(p->nrecent == 2 && strcmp(p->recent[0], "/r1") == 0 && strcmp(p->recent[1], "/r2") == 0);
    CHECK(p->has_last && strcmp(p->last, "/data/a") == 0);

    CHECK(files_prefs_parse(cjk, strlen(cjk), p) == 0);
    CHECK(p->nfav == 1 && strcmp(p->fav[0], "/data/\xe5\xae\x8c\xe8\x9b\x8b \"q\" \\b") == 0 && !p->has_last);
    /* round trip: out and back in unchanged */
    len = files_prefs_to_json(p, json, sizeof json);
    CHECK(len > 0);
    q = fresh();
    CHECK(files_prefs_parse(json, (size_t)len, q) == 0 && q->nfav == 1 && strcmp(q->fav[0], p->fav[0]) == 0);
    CHECK(strcmp(json, "{\"v\":1,\"favorites\":[\"/data/\xe5\xae\x8c\xe8\x9b\x8b \\\"q\\\" \\\\b\"],\"recent\":[],\"last\":null}") == 0);
    free(q);

    /* longer lists than the bounds are cut */
    {
        static char big[64 * 1024];
        size_t at = 0;
        int i;
        at += (size_t)snprintf(big + at, sizeof big - at, "{\"favorites\":[");
        for (i = 0; i < 70; i++) at += (size_t)snprintf(big + at, sizeof big - at, "%s\"/f%d\"", i ? "," : "", i);
        at += (size_t)snprintf(big + at, sizeof big - at, "],\"recent\":[");
        for (i = 0; i < 20; i++) at += (size_t)snprintf(big + at, sizeof big - at, "%s\"/r%d\"", i ? "," : "", i);
        at += (size_t)snprintf(big + at, sizeof big - at, "]}");
        CHECK(files_prefs_parse(big, at, p) == 0);
        CHECK(p->nfav == FILES_PREFS_FAV_MAX && p->nrecent == FILES_PREFS_RECENT_MAX);
        CHECK(strcmp(p->fav[49], "/f49") == 0 && strcmp(p->recent[11], "/r11") == 0);
    }
    free(p);
}

static void test_corruption(void) {
    static const char *bad[] = {
        "",
        "garbage",
        "{\"favorites\":[\"/a\",",                     /* cut short */
        "{\"favorites\":[\"/a\"],\"recent\":[\"/b\"]", /* no closing brace */
        "{\"favorites\":\"/a\"}",                      /* not an array */
        "{\"favorites\":[1,2]}",                       /* not strings */
        "{\"favorites\":[\"/a\"],\"last\":5}",         /* last not a string */
        "[\"/a\"]",
        "\x89PNG\r\n",
    };
    files_prefs_t *p = fresh();
    size_t i;
    int len;
    for (i = 0; i < sizeof bad / sizeof bad[0]; i++) {
        p->nfav = 3;
        CHECK(files_prefs_parse(bad[i], strlen(bad[i]), p) == 1);
        CHECK(p->nfav == 0 && p->nrecent == 0 && !p->has_last);
    }
    /* on disk: garbage reads as empty, and the next change replaces it with a whole file */
    put_file(g_file, "{\"favorites\":[\"/da", 18);
    CHECK(files_prefs_load(g_file, p) == 1 && p->nfav == 0);
    len = files_prefs_get(g_file, g_out, sizeof g_out);
    CHECK(len > 0 && strcmp(g_out, "{\"v\":1,\"favorites\":[],\"recent\":[],\"last\":null}") == 0);
    len = files_prefs_update(g_file, FILES_PREFS_PIN, "/data/games", g_out, sizeof g_out);
    CHECK(len > 0 && strstr(g_out, "\"favorites\":[\"/data/games\"]") != NULL);
    CHECK(files_prefs_load(g_file, p) == 0 && p->nfav == 1);
    /* a file over 64 KiB is not ours */
    {
        char *huge = malloc(FILES_PREFS_FILE_MAX + 10);
        memset(huge, ' ', FILES_PREFS_FILE_MAX + 10);
        memcpy(huge, "{\"favorites\":[\"/a\"]", 19);
        huge[FILES_PREFS_FILE_MAX + 9] = '}';
        put_file(g_file, huge, FILES_PREFS_FILE_MAX + 10);
        CHECK(files_prefs_load(g_file, p) == 1 && p->nfav == 0);
        /* exactly at the cap still reads */
        memset(huge, ' ', FILES_PREFS_FILE_MAX);
        memcpy(huge, "{\"favorites\":[\"/a\"]", 19);
        huge[FILES_PREFS_FILE_MAX - 1] = '}';
        put_file(g_file, huge, FILES_PREFS_FILE_MAX);
        CHECK(files_prefs_load(g_file, p) == 0 && p->nfav == 1);
        free(huge);
    }
    unlink(g_file);
    CHECK(files_prefs_load(g_file, p) == 1); /* missing */
    free(p);
}

static void test_file_cap(void) {
    /* paths of quotes double when escaped: 50 of them would pass 64 KiB */
    files_prefs_t *p = fresh();
    char path[FILES_PREFS_PATH_MAX];
    int i, rc = 0, pinned = 0, len;
    memset(path, '"', sizeof path);
    path[0] = '/';
    path[FILES_PREFS_PATH_MAX - 1] = '\0';
    unlink(g_file);
    for (i = 0; i < FILES_PREFS_FAV_MAX; i++) {
        path[1] = (char)('A' + i % 26);
        path[2] = (char)('A' + i / 26);
        rc = files_prefs_update(g_file, FILES_PREFS_PIN, path, g_out, sizeof g_out);
        if (rc < 0) break;
        pinned++;
    }
    CHECK(rc == FILES_PREFS_E_FULL);
    CHECK(pinned > 20 && pinned < FILES_PREFS_FAV_MAX);
    CHECK(files_prefs_load(g_file, p) == 0 && p->nfav == pinned); /* the refused pin left the file as it was */
    /* a visit makes room by dropping the oldest recent folders; the file stays under the cap */
    for (i = 0; i < 12; i++) {
        path[1] = (char)('a' + i);
        len = files_prefs_update(g_file, FILES_PREFS_VISIT, path, g_out, sizeof g_out);
        CHECK(len > 0 && (size_t)len <= FILES_PREFS_FILE_MAX);
    }
    CHECK(files_prefs_load(g_file, p) == 0 && p->nrecent >= 1 && p->recent[0][1] == 'l' && p->has_last);
    {
        struct stat st;
        CHECK(stat(g_file, &st) == 0 && (size_t)st.st_size <= FILES_PREFS_FILE_MAX);
    }
    unlink(g_file);
    free(p);
}

static void test_atomic_save(void) {
    files_prefs_t *p = fresh(), *q = fresh();
    char tmp[320], bad_file[320];
    files_prefs_pin(p, "/data/a");
    files_prefs_visit(p, "/data/a");
    CHECK(files_prefs_save(g_file, p) == 0);
    snprintf(tmp, sizeof tmp, "%s.tmp", g_file);
    CHECK(!exists(tmp));
    CHECK(files_prefs_load(g_file, q) == 0 && q->nfav == 1 && q->nrecent == 1 && strcmp(q->last, "/data/a") == 0);
    /* the temp path is taken by a folder: the save fails and the old file is untouched */
    CHECK(mkdir(tmp, 0755) == 0);
    files_prefs_pin(p, "/data/b");
    CHECK(files_prefs_save(g_file, p) == FILES_PREFS_E_IO);
    CHECK(files_prefs_update(g_file, FILES_PREFS_PIN, "/data/c", g_out, sizeof g_out) == FILES_PREFS_E_IO);
    CHECK(files_prefs_load(g_file, q) == 0 && q->nfav == 1);
    rmdir(tmp);
    /* a visit that changes nothing writes nothing: it succeeds even while a save could not */
    CHECK(files_prefs_update(g_file, FILES_PREFS_VISIT, "/data/v", g_out, sizeof g_out) > 0);
    CHECK(mkdir(tmp, 0755) == 0);
    CHECK(files_prefs_update(g_file, FILES_PREFS_VISIT, "/data/v/", g_out, sizeof g_out) > 0);
    CHECK(strstr(g_out, "\"last\":\"/data/v\"") != NULL);
    CHECK(files_prefs_update(g_file, FILES_PREFS_VISIT, "/data/w", g_out, sizeof g_out) == FILES_PREFS_E_IO);
    rmdir(tmp);
    /* a folder that does not exist: refused, nothing created */
    snprintf(bad_file, sizeof bad_file, "%s/missing/prefs.json", g_dir);
    CHECK(files_prefs_save(bad_file, p) == FILES_PREFS_E_IO);
    CHECK(files_prefs_update(g_file, 99, "/data", g_out, sizeof g_out) == FILES_PREFS_E_BAD_OP);
    CHECK(files_prefs_update(g_file, FILES_PREFS_PIN, "nope", g_out, sizeof g_out) == FILES_PREFS_E_BAD_PATH);
    unlink(g_file);
    free(p);
    free(q);
}

static void *writer(void *arg) {
    static char out[4][FILES_PREFS_FILE_MAX + 1];
    long id = (long)arg;
    char path[64];
    files_prefs_t *p = fresh();
    int i;
    for (i = 0; i < 39; i++) { /* 13 pin / visit / unpin rounds */
        int op = i % 3 == 0 ? FILES_PREFS_PIN : i % 3 == 1 ? FILES_PREFS_VISIT : FILES_PREFS_UNPIN;
        int len;
        snprintf(path, sizeof path, "/data/t%ld/%d", id, i / 3);
        len = files_prefs_update(g_file, op, path, out[id], sizeof out[id]);
        CHECK(len > 0);
        if (len > 0) CHECK(files_prefs_parse(out[id], (size_t)len, p) == 0);
    }
    free(p);
    return NULL;
}

static void test_concurrent(void) {
    pthread_t t[4];
    long i;
    files_prefs_t *p = fresh();
    unlink(g_file);
    for (i = 0; i < 4; i++) pthread_create(&t[i], NULL, writer, (void *)i);
    for (i = 0; i < 4; i++) pthread_join(t[i], NULL);
    CHECK(files_prefs_load(g_file, p) == 0);
    CHECK(p->nfav == 0); /* every pin was followed by its unpin */
    CHECK(p->nrecent == FILES_PREFS_RECENT_MAX && p->has_last);
    unlink(g_file);
    free(p);
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "%s", "/tmp/ps5upload-files-prefs-XXXXXX");
    if (!mkdtemp(g_dir)) {
        perror("mkdtemp");
        return 2;
    }
    snprintf(g_file, sizeof g_file, "%s/files-prefs.json", g_dir);
    test_clean_path();
    test_bounds();
    test_recent_order();
    test_parse_and_dedup();
    test_corruption();
    test_file_cap();
    test_atomic_save();
    test_concurrent();
    unlink(g_file);
    rmdir(g_dir);
    if (failures) {
        fprintf(stderr, "files_prefs_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("files_prefs_selftest: ok\n");
    return 0;
}
