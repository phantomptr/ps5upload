/* Host tests for the snapshots (backup.c), real files in a fresh temp folder: a snapshot of a tree
 * restores byte for byte to its original paths, the newest five of a tag are kept, the list stays
 * valid JSON inside its buffer whatever else lies in the folder, and a damaged manifest restores
 * only what it can. Build: see the root Makefile test-payload. */
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* backup.c as it is, against this test's own folder; its statics (rm_rf) clean up afterwards. */
static char g_backups_root[512];
#define BACKUPS_ROOT g_backups_root
#include "../src/backup.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- what backup.c takes from the rest of the helper ---- */

static int g_cancelled, g_denied;
static char g_root[256];

int mgmt_op_cancelled(void) { return g_cancelled; }
void mgmt_op_progress(uint64_t files, uint64_t bytes) {
    (void)files;
    (void)bytes;
}
/* The writable roots: anything under this test's folder, and nothing under the "denied" one. */
int is_path_allowed(const char *p) {
    if (g_denied && strstr(p, "/denied/")) return 0;
    return strncmp(p, g_root, strlen(g_root)) == 0;
}
int fsj_copy_atomic(const char *src, const char *dst, const fsj_hooks_t *h) {
    char buf[4096];
    int a, b, ok = 1;
    ssize_t n;
    (void)h;
    a = open(src, O_RDONLY);
    if (a < 0) return -1;
    b = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (b < 0) {
        close(a);
        return -1;
    }
    while ((n = read(a, buf, sizeof buf)) > 0)
        if (write(b, buf, (size_t)n) != n) ok = 0;
    close(a);
    close(b);
    return ok && n == 0 ? 0 : -1;
}

/* ---- small helpers ---- */

static void put(const char *path, const char *text) {
    FILE *f = fopen(path, "w");
    CHECK(f != NULL);
    if (!f) return;
    fputs(text, f);
    fclose(f);
}

static int same_text(const char *path, const char *text) {
    char buf[8192];
    FILE *f = fopen(path, "r");
    size_t n;
    if (!f) return 0;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return strcmp(buf, text) == 0;
}

static void mk(const char *fmt, const char *arg) {
    char p[1024];
    snprintf(p, sizeof p, fmt, g_root, arg);
    mkdir(p, 0777);
}

/* A snapshot folder as an earlier run left it, with one file. */
static void plant_snapshot(const char *tag, long long ts) {
    char p[1024];
    snprintf(p, sizeof p, "%s/%s", BACKUPS_ROOT, tag);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/%s/%lld", BACKUPS_ROOT, tag, ts);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/%s/%lld/.manifest", BACKUPS_ROOT, tag, ts);
    put(p, "f\t/nowhere/f\n");
    snprintf(p, sizeof p, "%s/%s/%lld/f", BACKUPS_ROOT, tag, ts);
    put(p, "12345");
}

static int snapshot_exists(const char *tag, long long ts) {
    char p[1024];
    struct stat st;
    snprintf(p, sizeof p, "%s/%s/%lld", BACKUPS_ROOT, tag, ts);
    return stat(p, &st) == 0;
}

static int count_snapshots(const char *tag) {
    char p[1024];
    DIR *d;
    struct dirent *e;
    int n = 0;
    snprintf(p, sizeof p, "%s/%s", BACKUPS_ROOT, tag);
    d = opendir(p);
    if (!d) return 0;
    while ((e = readdir(d)))
        if (e->d_name[0] != '.') n++;
    closedir(d);
    return n;
}

/* A strict-enough check of the reply: balanced, closed, no raw control bytes, string quotes paired. */
static int json_ok(const char *s, size_t n) {
    int depth = 0, in = 0;
    if (n < 2 || s[0] != '{' || s[n - 1] != '}' || strlen(s) != n) return 0;
    for (size_t i = 0; i < n; i++) {
        char c = s[i];
        if ((unsigned char)c < 0x20) return 0;
        if (in) {
            if (c == '\\') i++;
            else if (c == '"') in = 0;
        } else if (c == '"') {
            in = 1;
        } else if (c == '{' || c == '[') {
            depth++;
        } else if (c == '}' || c == ']') {
            depth--;
        }
    }
    return depth == 0 && !in;
}

/* ---- the tests ---- */

static void round_trip(void) {
    char src[512], a[600], b[600], c[600];
    int64_t ts = 0;
    int files = 0, restored = 0;
    uint64_t bytes = 0;

    snprintf(src, sizeof src, "%s/save", g_root);
    mkdir(src, 0777);
    snprintf(a, sizeof a, "%s/one", src);
    put(a, "first file");
    mk("%s/save/%s", "sub");
    snprintf(b, sizeof b, "%s/sub/two", src);
    put(b, "second file, longer");
    snprintf(c, sizeof c, "%s/sub/empty", src);
    put(c, "");

    CHECK(backup_snapshot("t1", src, &ts, &files, &bytes) == 0);
    CHECK(files == 3);
    CHECK(bytes == strlen("first file") + strlen("second file, longer"));
    CHECK(ts > 0);

    /* Overwritten and deleted: a restore puts back what was there. */
    put(a, "changed");
    unlink(b);
    CHECK(backup_restore("t1", ts, &restored) == 0);
    CHECK(restored == 3);
    CHECK(same_text(a, "first file"));
    CHECK(same_text(b, "second file, longer"));
    CHECK(same_text(c, ""));

    /* Listed with what it holds. */
    {
        char buf[4096], want[128];
        size_t w = 0;
        CHECK(backup_list("t1", buf, sizeof buf, &w) == 0);
        CHECK(json_ok(buf, w));
        snprintf(want, sizeof want, "{\"tag\":\"t1\",\"timestamp\":%lld,\"files\":3,\"bytes\":%llu}",
                 (long long)ts, (unsigned long long)bytes);
        CHECK(strstr(buf, want) != NULL);
    }

    /* Deleted: gone, and the tag folder with it. */
    CHECK(backup_delete("t1", ts) == 0);
    CHECK(!snapshot_exists("t1", ts));
    CHECK(count_snapshots("t1") == 0);
}

/* A source with nothing in it, or that does not exist, is no snapshot. */
static void nothing_to_back_up(void) {
    char empty[512];
    int64_t ts = 0;
    int files = 7;
    uint64_t bytes = 0;
    snprintf(empty, sizeof empty, "%s/nothing", g_root);
    mkdir(empty, 0777);
    CHECK(backup_snapshot("t2", empty, &ts, &files, &bytes) == -1);
    CHECK(count_snapshots("t2") == 0);
    CHECK(backup_snapshot("t2", "/no/such/path", &ts, &files, &bytes) == -1);
    CHECK(backup_snapshot("bad tag", empty, &ts, &files, &bytes) == -1);
    CHECK(backup_snapshot("../up", empty, &ts, &files, &bytes) == -1);
}

/* The newest five stay, the oldest go, and the screen's promise holds. */
static void keeps_the_newest_five(void) {
    char one[512];
    int64_t ts = 0;
    int files = 0;
    uint64_t bytes = 0;
    long long base = 1000000;

    snprintf(one, sizeof one, "%s/onefile", g_root);
    put(one, "x");
    for (int i = 0; i < 9; i++) plant_snapshot("t3", base + i);
    plant_snapshot("t3other", base); /* another tag is not touched */
    CHECK(count_snapshots("t3") == 9);

    CHECK(backup_snapshot("t3", one, &ts, &files, &bytes) == 0);
    CHECK(count_snapshots("t3") == 5);
    CHECK(snapshot_exists("t3", ts));
    for (int i = 0; i < 5; i++) CHECK(!snapshot_exists("t3", base + i));
    for (int i = 5; i < 9; i++) CHECK(snapshot_exists("t3", base + i));
    CHECK(snapshot_exists("t3other", base));

    /* A failed snapshot deletes nothing. */
    CHECK(backup_snapshot("t3", "/no/such/path", &ts, &files, &bytes) == -1);
    CHECK(count_snapshots("t3") == 5);
}

/* The clock moved back: the snapshot just made has the lowest time, and it is still the one kept. */
static void keeps_the_new_one_whatever_the_clock_says(void) {
    char one[512];
    int64_t ts = 0;
    int files = 0;
    uint64_t bytes = 0;
    long long far = (long long)time(NULL) + 1000000;

    snprintf(one, sizeof one, "%s/onefile", g_root);
    put(one, "x");
    for (int i = 0; i < 5; i++) plant_snapshot("t4", far + i);
    CHECK(backup_snapshot("t4", one, &ts, &files, &bytes) == 0);
    CHECK(snapshot_exists("t4", ts));
    CHECK(count_snapshots("t4") == 5);
    CHECK(!snapshot_exists("t4", far));
    CHECK(snapshot_exists("t4", far + 1));
}

/* Snapshots are named by the second. Two of one tag inside a second were given the SAME folder: the
 * second wrote into the first, and one that was then cancelled or copied nothing removed the folder,
 * and the first snapshot with it. Each gets a folder of its own, and a failed one removes only its own. */
static void two_in_one_second_do_not_share_a_folder(void) {
    char a[512], b[512], empty[512];
    int64_t ts1 = 0, ts2 = 0, ts3 = 0;
    int files = 0, restored = 0;
    uint64_t bytes = 0;

    snprintf(a, sizeof a, "%s/fast-a", g_root);
    snprintf(b, sizeof b, "%s/fast-b", g_root);
    snprintf(empty, sizeof empty, "%s/fast-empty", g_root);
    put(a, "the first");
    put(b, "the second one");
    mkdir(empty, 0777);

    CHECK(backup_snapshot("t5", a, &ts1, &files, &bytes) == 0);
    CHECK(backup_snapshot("t5", b, &ts2, &files, &bytes) == 0);
    CHECK(ts1 != ts2);
    CHECK(count_snapshots("t5") == 2);
    CHECK(files == 1 && bytes == strlen("the second one")); /* only its own file, not the first's too */

    /* Cancelled straight after: it goes, the two before it stay. */
    g_cancelled = 1;
    CHECK(backup_snapshot("t5", a, &ts3, &files, &bytes) == BACKUP_CANCELLED);
    g_cancelled = 0;
    CHECK(count_snapshots("t5") == 2);
    /* Nothing to copy straight after: the same. */
    CHECK(backup_snapshot("t5", empty, &ts3, &files, &bytes) == -1);
    CHECK(count_snapshots("t5") == 2);
    CHECK(snapshot_exists("t5", ts1) && snapshot_exists("t5", ts2));

    /* And each still restores what it took. */
    put(a, "changed");
    put(b, "changed");
    CHECK(backup_restore("t5", ts1, &restored) == 0 && restored == 1);
    CHECK(same_text(a, "the first"));
    CHECK(same_text(b, "changed"));
    CHECK(backup_restore("t5", ts2, &restored) == 0 && restored == 1);
    CHECK(same_text(b, "the second one"));
}

/* Any folder can lie under the backups folder: none of it may break the reply. */
static void list_survives_strange_folders(void) {
    char p[1024], name[260], big[4096];
    size_t w = 0;

    plant_snapshot("okay", 1234);
    /* A name no tag can have: long, with a quote, a backslash, a control byte. */
    memset(name, 'L', 250);
    name[250] = 0;
    snprintf(p, sizeof p, "%s/%s", BACKUPS_ROOT, name);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/%s/1", BACKUPS_ROOT, name);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/quo\"te", BACKUPS_ROOT);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/quo\"te/2", BACKUPS_ROOT);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/back\\slash", BACKUPS_ROOT);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/ctrl\x01", BACKUPS_ROOT);
    mkdir(p, 0777);
    /* Inside a good tag: something that is not a snapshot, and a plain file. */
    snprintf(p, sizeof p, "%s/okay/not-a-time", BACKUPS_ROOT);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/okay/5678", BACKUPS_ROOT);
    put(p, "a file, not a folder");

    CHECK(backup_list(NULL, big, sizeof big, &w) == 0);
    CHECK(json_ok(big, w));
    CHECK(strstr(big, "\"tag\":\"okay\",\"timestamp\":1234") != NULL);
    CHECK(strstr(big, "LLLL") == NULL);
    CHECK(strstr(big, "quo") == NULL);
    CHECK(strstr(big, "not-a-time") == NULL);
    CHECK(strstr(big, "5678") == NULL);
    CHECK(strstr(big, "\"timestamp\":0") == NULL);

    /* Only the one tag asked for. */
    CHECK(backup_list("t3other", big, sizeof big, &w) == 0);
    CHECK(json_ok(big, w));
    CHECK(strstr(big, "okay") == NULL && strstr(big, "t3other") != NULL);
    CHECK(backup_list("no-such-tag", big, sizeof big, &w) == 0);
    CHECK(strcmp(big, "{\"snapshots\":[]}") == 0 && w == strlen(big));
}

/* With more snapshots than fit: fewer entries, valid JSON, and never past the buffer. */
static void list_stays_inside_its_buffer(void) {
    char tag[33];
    char *buf;
    size_t caps[] = {32, 64, 128, 200, 256, 512, 1024, 2048, 4096};

    for (int t = 0; t < 40; t++) {
        snprintf(tag, sizeof tag, "many-%02d-aaaaaaaaaaaaaaaaaaaaaaaa", t);
        for (int s = 0; s < 4; s++) plant_snapshot(tag, 1700000000000000000ll + s);
    }
    for (size_t i = 0; i < sizeof caps / sizeof caps[0]; i++) {
        size_t cap = caps[i], w = 0;
        buf = malloc(cap + 16);
        CHECK(buf != NULL);
        if (!buf) return;
        memset(buf, 0x5a, cap + 16);
        CHECK(backup_list(NULL, buf, cap, &w) == 0);
        CHECK(w < cap);
        CHECK(json_ok(buf, w));
        for (size_t k = cap; k < cap + 16; k++) CHECK((unsigned char)buf[k] == 0x5a);
        free(buf);
    }
    /* Too small for even the empty list: refused, nothing written past it. */
    {
        char tiny[8];
        size_t w = 99;
        CHECK(backup_list(NULL, tiny, sizeof tiny, &w) == -1);
        CHECK(w == 0);
    }
}

/* A manifest damaged or edited by hand: what is good is restored, what is not is left alone. */
static void damaged_manifest(void) {
    char snap[1024], p[1200], ok_dst[600], denied_dst[600], text[2048];
    int restored = -1;

    snprintf(snap, sizeof snap, "%s/t5/2000", BACKUPS_ROOT);
    snprintf(p, sizeof p, "%s/t5", BACKUPS_ROOT);
    mkdir(p, 0777);
    mkdir(snap, 0777);
    snprintf(p, sizeof p, "%s/good", snap);
    put(p, "good bytes");
    snprintf(p, sizeof p, "%s/evil", snap);
    put(p, "evil bytes");

    snprintf(ok_dst, sizeof ok_dst, "%s/restored-good", g_root);
    mk("%s/%s", "denied");
    snprintf(denied_dst, sizeof denied_dst, "%s/denied/restored-evil", g_root);
    snprintf(text, sizeof text,
             "no tab on this line\n"
             "good\t%s\n"
             "evil\t%s\n"
             "evil\t/outside/the/roots\n"
             "missing-file\t%s/restored-missing\n"
             "truncated-with-no-end",
             ok_dst, denied_dst, g_root);
    snprintf(p, sizeof p, "%s/.manifest", snap);
    put(p, text);

    g_denied = 1;
    CHECK(backup_restore("t5", 2000, &restored) == 0);
    g_denied = 0;
    CHECK(restored == 1);
    CHECK(same_text(ok_dst, "good bytes"));
    CHECK(access(denied_dst, F_OK) != 0);

    /* No snapshot, or a bad tag: a refusal, nothing restored. */
    CHECK(backup_restore("t5", 9999, &restored) == -1);
    CHECK(backup_restore("../t5", 2000, &restored) == -1);

    /* A truncated file is a smaller list, not a crash; an empty manifest restores nothing. */
    snprintf(p, sizeof p, "%s/.manifest", snap);
    put(p, "");
    restored = -1;
    CHECK(backup_restore("t5", 2000, &restored) == 0);
    CHECK(restored == 0);
    unlink(p);
    CHECK(backup_restore("t5", 2000, &restored) == -1);
}

int main(void) {
    char tmpl[] = "/tmp/ps5upload-backup-XXXXXX";
    if (!mkdtemp(tmpl)) {
        perror("mkdtemp");
        return 1;
    }
    snprintf(g_root, sizeof g_root, "%s", tmpl);
    snprintf(g_backups_root, sizeof g_backups_root, "%s/backups", tmpl);
    backup_init();
    round_trip();
    nothing_to_back_up();
    keeps_the_newest_five();
    keeps_the_new_one_whatever_the_clock_says();
    two_in_one_second_do_not_share_a_folder();
    list_survives_strange_folders();
    list_stays_inside_its_buffer();
    damaged_manifest();
    (void)rm_rf(tmpl);
    if (failures) {
        fprintf(stderr, "backup_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("backup_selftest: ok\n");
    return 0;
}
