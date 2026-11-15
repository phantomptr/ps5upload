/* Host test for the helper's side of converting on the console (src/console_convert.c and
 * src/console_convert_wire.c): the request it writes, how it reads a status (also one cut short or
 * caught half written), Stop, a converter that will not stop or dies, the busy rules, and what a
 * helper that died mid-conversion leaves for the next start to remove.
 *
 * The real operation runs here against a temporary folder; a thread stands in for the converter
 * payload and the ELF loader (CONSOLE_CONVERT_SELFTEST). Build: see the root Makefile test-payload. */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "console_convert.h"
#include "fs_jobs.h"
#include "webui_codec.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- what the helper gives console_convert.c ---- */

struct ava1_op_ctx {
    volatile int cancelled;
    char message[256];
    char result[CONVERT_RESULT_MAX + 1];
    size_t result_len;
    uint64_t total, done;
};

int ava1_op_cancelled(const ava1_op_ctx_t *c) { return c->cancelled; }
void ava1_op_set_total(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes) {
    (void)files;
    c->total = bytes;
}
void ava1_op_add(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes) {
    (void)files;
    c->done += bytes;
}
void ava1_op_message(ava1_op_ctx_t *c, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(c->message, sizeof c->message, fmt, ap);
    va_end(ap);
}
int ava1_op_set_result(ava1_op_ctx_t *c, const void *body, size_t len) {
    if (len > CONVERT_RESULT_MAX) return -1;
    memcpy(c->result, body, len);
    c->result[len] = 0;
    c->result_len = len;
    return 0;
}
int ava1_op_register(uint8_t op, ava1_op_fn fn, void *arg) {
    (void)op;
    (void)fn;
    (void)arg;
    return 0;
}

int fsj_json_str(const char *json, const char *key, char *out, size_t cap) {
    return webui_json_str(json, strlen(json), key, out, cap) == 1;
}
int fsj_json_u64(const char *json, const char *key, uint64_t *out) {
    int b = 0;
    if (webui_json_u64(json, strlen(json), key, out) == 1) return 1;
    if (webui_json_bool(json, strlen(json), key, &b) != 1) return 0;
    *out = (uint64_t)b;
    return 1;
}
int fsj_normalize_path(char *path) { return path[0] == '/' && !strstr(path, "/..") ? 0 : -1; }
int fsj_path_is_root(const char *path) { return strcmp(path, "/") == 0; }
int fsj_rm_rf(const char *path, int depth, const fsj_hooks_t *h) {
    char cmd[1200];
    (void)depth;
    (void)h;
    /* Only ever a folder this test made under its own temporary root. */
    snprintf(cmd, sizeof cmd, "rm -r '%s'", path);
    return system(cmd) == 0 ? 0 : -1;
}

static char g_root[256];
int is_path_allowed(const char *p) { return strncmp(p, g_root, strlen(g_root)) == 0; }
int path_tree_op_refused(const char *p) { return strstr(p, "/refused") != NULL; }

static int g_installing, g_extracting, g_ticks;
/* What console_convert_building() said each time the build's busy check read the others' flags:
 * the build's own must already be up (set first, then theirs read). -1: not asked yet. */
static int g_building_when_installing_read = -1, g_building_when_extracting_read = -1;
int webui_pkg_installing(void) {
    g_building_when_installing_read = console_convert_building();
    return g_installing;
}
int archive_extract_running(void) {
    g_building_when_extracting_read = console_convert_building();
    return g_extracting;
}
pthread_mutex_t sony_api_lock = PTHREAD_MUTEX_INITIALIZER;
int sceSystemServicePowerTick(void) {
    g_ticks++;
    return 0;
}
unsigned char ps5upload_convert_elf[1] = {0x7f};
unsigned int ps5upload_convert_elf_len = 1;

/* ---- the stand-in converter ---- */

typedef enum {
    FAKE_INSPECT,
    FAKE_BUILD_UNTIL_CANCEL,
    FAKE_HANG,
    FAKE_NEVER,
    FAKE_DIED,
    FAKE_FAILS,
    FAKE_STAGES,
    FAKE_PID,   /* reports pid g_fake_pid, then Stop, and never ends */
    FAKE_STOLEN /* another job takes the folder (its record and status), then this one fails */
} fake_t;
static fake_t g_fake;
static long long g_fake_pid;
/* The pid the in-thread stand-ins report: a sleeping child of this test, since the helper never
 * takes its own pid for the converter's. */
static pid_t g_alive;
static char g_dir[300], g_request[4096];
static ava1_op_ctx_t *g_ctx;
static pid_t g_child;
static pthread_t g_thread;
static int g_thread_started;

static void path_in(char *out, size_t cap, const char *name) { snprintf(out, cap, "%s/%s", g_dir, name); }

static void put_file(const char *path, const char *text) {
    FILE *f = fopen(path, "w");
    if (!f) return;
    fputs(text, f);
    fclose(f);
}

/* As the converter writes it: a temporary file in the folder, renamed over status.json. */
static void put_status(const char *fmt, ...) {
    char tmp[400], final[400], text[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(text, sizeof text, fmt, ap);
    va_end(ap);
    path_in(tmp, sizeof tmp, "status.json.tmp");
    path_in(final, sizeof final, "status.json");
    put_file(tmp, text);
    rename(tmp, final);
}

static int exists(const char *path) {
    struct stat st;
    return lstat(path, &st) == 0;
}

static char g_rtdir[320];

/* A runtime folder for `version`, holding `file` (none when NULL). */
static void put_runtime(const char *version, const char *file) {
    char p[400];
    mkdir(g_rtdir, 0700);
    snprintf(p, sizeof p, "%s/ampr-emu-ng-%s", g_rtdir, version);
    mkdir(p, 0700);
    if (!file) return;
    snprintf(p, sizeof p, "%s/ampr-emu-ng-%s/%s", g_rtdir, version, file);
    put_file(p, "EMULATOR");
}

static void *fake_main(void *arg) {
    char req[400], id[64], cancel[400], status[400];
    FILE *f;
    size_t n;
    (void)arg;
    path_in(req, sizeof req, "request.json");
    path_in(cancel, sizeof cancel, "cancel");
    path_in(status, sizeof status, "status.json");
    f = fopen(req, "r");
    if (!f) return NULL;
    n = fread(g_request, 1, sizeof g_request - 1, f);
    fclose(f);
    g_request[n] = 0;
    unlink(req);
    if (webui_json_str(g_request, n, "id", id, sizeof id) != 1) return NULL;
    switch (g_fake) {
    case FAKE_NEVER: break;
    case FAKE_INSPECT:
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"inspect\",\"state\":\"running\"}\n", id, (long)g_alive);
        usleep(300 * 1000);
        /* What a reader could catch if the file were written in place: half a status. The helper
         * must read it as "nothing new", not as a failure. */
        put_file(status, "{\"id\":\"");
        usleep(300 * 1000);
        put_file(status, "\x01\x02 not json at all");
        usleep(300 * 1000);
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"inspect\",\"state\":\"done\","
                   "\"result\":{\"path\":\"/x/a \\\"b\\\" }\",\"files\":3,\"warnings\":[\"no {eboot}\"]}}\n",
                   id, (long)g_alive);
        break;
    case FAKE_FAILS:
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"failed\",\"cause\":\"convert_build_failed\","
                   "\"detail\":\"read all 5 bytes\"}\n",
                   id, (long)g_alive);
        break;
    case FAKE_BUILD_UNTIL_CANCEL: {
        unsigned done = 0;
        while (!exists(cancel)) {
            done += 1000;
            put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"running\",\"stage\":\"read\","
                       "\"bytes_done\":%u,\"bytes_total\":1000000}\n",
                       id, (long)g_alive, done);
            usleep(100 * 1000);
            if (done >= 3000) g_ctx->cancelled = 1; /* the user presses Stop */
        }
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"failed\",\"cause\":\"convert_cancelled\"}\n", id,
                   (long)g_alive);
        break;
    }
    case FAKE_STAGES: {
        /* Each stage counts from 0 against its own total, as the converter reports them. */
        static const struct { const char *stage; unsigned done, total; } steps[] = {
            {"pack", 100, 100}, {"write", 50, 200}, {"write", 200, 200}, {"verify", 30, 30},
        };
        for (size_t i = 0; i < sizeof steps / sizeof steps[0]; i++) {
            put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"running\",\"stage\":\"%s\","
                       "\"bytes_done\":%u,\"bytes_total\":%u}\n",
                       id, (long)g_alive, steps[i].stage, steps[i].done, steps[i].total);
            usleep(700 * 1000); /* slower than the helper reads, so it sees every step */
        }
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"done\",\"result\":{\"path\":\"x\"}}\n", id,
                   (long)g_alive);
        break;
    }
    case FAKE_STOLEN: {
        char rec[400], text[600];
        path_in(rec, sizeof rec, "job.rec");
        cc_record_format(text, sizeof text, 0, (long)getppid(), "feed", "/elsewhere/other.exfat");
        put_file(rec, text);
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"failed\",\"cause\":\"convert_build_failed\"}\n",
                   id, (long)g_alive);
        break;
    }
    case FAKE_PID:
        put_status("{\"id\":\"%s\",\"pid\":%lld,\"op\":\"build\",\"state\":\"running\",\"stage\":\"write\"}\n", id,
                   g_fake_pid);
        g_ctx->cancelled = 1;
        break;
    case FAKE_HANG:
    case FAKE_DIED:
        /* A process of its own, so the helper has a pid it may kill. */
        g_child = fork();
        if (g_child == 0) {
            if (g_fake == FAKE_DIED) _exit(0);
            for (;;) pause();
        }
        if (g_fake == FAKE_DIED) waitpid(g_child, NULL, 0);
        put_status("{\"id\":\"%s\",\"pid\":%ld,\"op\":\"build\",\"state\":\"running\",\"stage\":\"write\"}\n", id,
                   (long)g_child);
        if (g_fake == FAKE_HANG) g_ctx->cancelled = 1; /* Stop, which this one ignores */
        break;
    }
    return NULL;
}

static int fake_launch(void) {
    g_thread_started = pthread_create(&g_thread, NULL, fake_main, NULL) == 0;
    return g_thread_started ? 0 : -1;
}

/* Runs the operation with `args` against the stand-in. */
static int run_op(fake_t fake, const char *args, ava1_op_ctx_t *c) {
    int st;
    memset(c, 0, sizeof *c);
    g_fake = fake;
    g_ctx = c;
    g_child = 0;
    g_thread_started = 0;
    g_request[0] = 0;
    st = console_convert_op(NULL, c, (const uint8_t *)args, strlen(args));
    if (g_thread_started) pthread_join(g_thread, NULL);
    return st;
}

/* A literal status text, its length taken from the literal (one of them holds a NUL). */
#define PARSE(text) cc_status_parse(text, sizeof(text) - 1, "ab", &s)

static void test_wire(void) {
    char buf[4096], got[1024];
    cc_build_t b = {"/data/out \"q\".exfat", "exfat", 1, 1, 0, NULL};
    cc_status_t s;
    const char *full = "{\"id\":\"ab\",\"pid\":77,\"op\":\"build\",\"state\":\"running\",\"stage\":\"write\","
                       "\"bytes_done\":5,\"bytes_total\":9,\"result\":{\"a\":\"}\",\"b\":[1,{\"c\":2}]}}\n";
    size_t n = strlen(full), i;
    int len;

    /* The request: every field comes back as it went in, whatever is in the path. */
    len = cc_request_json(buf, sizeof buf, "ab12", "build", "/data/g\xc3\xa9 \\ \"x\"\n", &b);
    CHECK(len > 0 && buf[len - 1] == '\n');
    CHECK(webui_json_str(buf, (size_t)len, "id", got, sizeof got) == 1 && strcmp(got, "ab12") == 0);
    CHECK(webui_json_str(buf, (size_t)len, "op", got, sizeof got) == 1 && strcmp(got, "build") == 0);
    CHECK(webui_json_str(buf, (size_t)len, "path", got, sizeof got) == 1 && strcmp(got, "/data/g\xc3\xa9 \\ \"x\"\n") == 0);
    CHECK(webui_json_str(buf, (size_t)len, "dest", got, sizeof got) == 1 && strcmp(got, b.dest) == 0);
    CHECK(webui_json_str(buf, (size_t)len, "verify", got, sizeof got) == 1 && strcmp(got, "full") == 0);
    {
        int z = 0;
        CHECK(webui_json_bool(buf, (size_t)len, "zstd", &z) == 1 && z == 1);
    }
    {
        /* The level goes in with zstd only, and only when one was chosen. */
        uint64_t lv = 0;
        CHECK(webui_json_u64(buf, (size_t)len, "zstd_level", &lv) == 0);
        b.zstd_level = 19;
        len = cc_request_json(buf, sizeof buf, "ab12", "build", "/data/g", &b);
        CHECK(len > 0 && webui_json_u64(buf, (size_t)len, "zstd_level", &lv) == 1 && lv == 19);
        b.zstd = 0;
        len = cc_request_json(buf, sizeof buf, "ab12", "build", "/data/g", &b);
        CHECK(len > 0 && webui_json_u64(buf, (size_t)len, "zstd_level", &lv) == 0);
        CHECK(cc_zstd_level_ok(1) && cc_zstd_level_ok(22) && !cc_zstd_level_ok(0) && !cc_zstd_level_ok(23));
    }
    {
        /* The runtime's path goes with zstd only. */
        b.zstd = 1;
        b.zstd_runtime = "/data/ps5upload/convert/runtime/ampr-emu-ng-0.4.1/libSceAmpr.sprx";
        len = cc_request_json(buf, sizeof buf, "ab12", "build", "/data/g", &b);
        CHECK(len > 0 && webui_json_str(buf, (size_t)len, "zstd_runtime", got, sizeof got) == 1 &&
              strcmp(got, b.zstd_runtime) == 0);
        b.zstd = 0;
        len = cc_request_json(buf, sizeof buf, "ab12", "build", "/data/g", &b);
        CHECK(len > 0 && webui_json_str(buf, (size_t)len, "zstd_runtime", got, sizeof got) == 0);
        b.zstd_runtime = NULL;
    }
    len = cc_request_json(buf, sizeof buf, "ab12", "inspect", "/data/g", NULL);
    CHECK(len > 0 && webui_json_str(buf, (size_t)len, "dest", got, sizeof got) == 0);

    /* Runtime folders and their versions, compared as the engine compares them. */
    CHECK(cc_runtime_folder_version("ampr-emu-ng-0.4.1", got, sizeof got) == 1 && strcmp(got, "0.4.1") == 0);
    CHECK(cc_runtime_folder_version("ampr-emu-ng-0.5.0-rc1", got, sizeof got) == 1 && strcmp(got, "0.5.0-rc1") == 0);
    CHECK(!cc_runtime_folder_version("ampr-emu-ng-", got, sizeof got));
    CHECK(!cc_runtime_folder_version("ampr_emu-0.4.1", got, sizeof got));
    CHECK(!cc_runtime_folder_version("ampr-emu-ng-0 4", got, sizeof got));
    CHECK(!cc_runtime_folder_version("ampr-emu-ng-0.4.1", got, 5)); /* does not fit */
    CHECK(cc_version_older("0.4.0", "0.4.1") && !cc_version_older("0.4.1", "0.4.0"));
    CHECK(cc_version_older("0.9.0", "0.10.0") && !cc_version_older("0.10.0", "0.9.0"));
    CHECK(cc_version_older("0.4", "0.4.0") && !cc_version_older("0.4.0", "0.4"));
    CHECK(!cc_version_older("0.4.1", "0.4.1"));
    CHECK(cc_version_older("0.5.0-rc1", "0.5.1") && !cc_version_older("0.5.0-rc1", "0.5.0"));
    CHECK(cc_version_older("99999999999999999999999.1", "0.2")); /* too large a part counts as 0 */

    /* The inspection's answer, with the runtime added. */
    {
        const char *r = "{\"path\":\"/a\",\"files\":3} \n";
        char out[128];
        len = cc_result_with_runtime(out, sizeof out, r, strlen(r), "0.4.1");
        CHECK(len > 0 && strcmp(out, "{\"path\":\"/a\",\"files\":3,\"zstd_runtime\":\"0.4.1\"}") == 0 &&
              (size_t)len == strlen(out));
        len = cc_result_with_runtime(out, sizeof out, r, strlen(r), NULL);
        CHECK(len > 0 && strcmp(out, "{\"path\":\"/a\",\"files\":3,\"zstd_runtime\":null}") == 0);
        len = cc_result_with_runtime(out, sizeof out, "{ }", 3, "1");
        CHECK(len > 0 && strcmp(out, "{ \"zstd_runtime\":\"1\"}") == 0);
        CHECK(cc_result_with_runtime(out, sizeof out, "{}", 2, NULL) > 0 && strcmp(out, "{\"zstd_runtime\":null}") == 0);
        CHECK(cc_result_with_runtime(out, sizeof out, "[1]", 3, NULL) == -1);
        CHECK(cc_result_with_runtime(out, sizeof out, "{", 1, NULL) == -1);
        /* Too small for the member: refused, never cut. */
        CHECK(cc_result_with_runtime(out, 30, r, strlen(r), "0.4.1") == -1);
        CHECK(cc_result_with_runtime(out, 30, r, strlen(r), NULL) == -1);
    }
    CHECK(cc_request_json(buf, 24, "ab12", "inspect", "/data/a/long/path", NULL) == -1);

    /* A whole status. */
    CHECK(cc_status_parse(full, n, "ab", &s) == 1);
    CHECK(strcmp(s.state, "running") == 0 && strcmp(s.stage, "write") == 0 && s.pid == 77);
    CHECK(s.bytes_done == 5 && s.bytes_total == 9);
    CHECK(s.result && s.result_len == strlen("{\"a\":\"}\",\"b\":[1,{\"c\":2}]}") &&
          strncmp(s.result, "{\"a\":\"}\"", 8) == 0);
    /* A pid that cannot be the converter's reads as none: cast to pid_t these would be -1 (every
     * process), 1 (init) or 0 (the helper's own process group). */
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":4294967295}") == 1 && s.pid == 0);
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":4294967297}") == 1 && s.pid == 0);
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":2147483648}") == 1 && s.pid == 0);
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":1}") == 1 && s.pid == 0);
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":0}") == 1 && s.pid == 0);
    CHECK(PARSE("{\"id\":\"ab\",\"state\":\"running\",\"pid\":2147483647}") == 1 && s.pid == 2147483647u);
    CHECK(!cc_pid_ok(0) && !cc_pid_ok(1) && cc_pid_ok(2) && !cc_pid_ok(0xFFFFFFFFu) && !cc_pid_ok(0x100000001ull));
    /* Another job's, an unknown state, no state. */
    CHECK(cc_status_parse(full, n, "zz", &s) == 0);
    CHECK(cc_status_parse("{\"id\":\"ab\",\"state\":\"paused\"}", 28, "ab", &s) == 0);
    CHECK(cc_status_parse("{\"id\":\"ab\"}", 11, "ab", &s) == 0);
    /* Cut anywhere, it is never believed (and never read past its end). */
    for (i = 0; i + 1 < n; i++) {
        char *cut = malloc(i ? i : 1);
        memcpy(cut, full, i);
        CHECK(cc_status_parse(cut, i, "ab", &s) == 0);
        free(cut);
    }
    /* Garbage, an embedded NUL, something after the object, a result that is not an object. */
    CHECK(cc_status_parse("\x01\x02 not json", 11, "ab", &s) == 0);
    CHECK(cc_status_parse("{\"id\":\"ab\",\"state\":\"done\"\0}", 27, "ab", &s) == 0);
    CHECK(cc_status_parse("{\"id\":\"ab\",\"state\":\"done\"}{", 27, "ab", &s) == 0);
    CHECK(cc_status_parse("{\"id\":\"ab\",\"state\":\"done\",\"result\":\"x\"}", 39, "ab", &s) == 1 && s.result == NULL);
    /* A result nested under another key is not the result. */
    CHECK(cc_status_parse("{\"id\":\"ab\",\"state\":\"done\",\"x\":{\"result\":{}}}", 44, "ab", &s) == 1 && s.result == NULL);
    /* A detail longer than the helper keeps reads as none; the state still counts. */
    {
        char big[1024];
        int w = snprintf(big, sizeof big, "{\"id\":\"ab\",\"state\":\"failed\",\"cause\":\"convert_x\",\"detail\":\"%0600d\"}", 1);
        CHECK(cc_status_parse(big, (size_t)w, "ab", &s) == 1 && strcmp(s.cause, "convert_x") == 0 && !s.detail[0]);
    }

    /* The record. */
    {
        unsigned long long xpid = 0;
        long hpid = 0;
        char dest[64], rid[CC_JOB_ID_MAX];
#define RPARSE(text, cap) cc_record_parse(text, &xpid, &hpid, rid, sizeof rid, dest, cap)
        len = cc_record_format(buf, sizeof buf, 4242, 77, "0123abcd", "/data/out.exfat");
        CHECK(len > 0 && RPARSE(buf, sizeof dest) == 1);
        CHECK(xpid == 4242 && hpid == 77 && strcmp(rid, "0123abcd") == 0 && strcmp(dest, "/data/out.exfat") == 0);
        len = cc_record_format(buf, sizeof buf, 0, 2, "ab", "");
        CHECK(len > 0 && RPARSE(buf, sizeof dest) == 1 && xpid == 0 && !dest[0]);
        CHECK(cc_record_format(buf, sizeof buf, 1, 2, "ab", "/data/a\nb") == -1);
        /* An id that could split the record, or none at all, is never written. */
        CHECK(cc_record_format(buf, sizeof buf, 1, 2, "a b", "/d") == -1);
        CHECK(cc_record_format(buf, sizeof buf, 1, 2, "", "/d") == -1);
        CHECK(cc_record_format(buf, sizeof buf, 1, 2, "AB", "/d") == -1);
        /* A record from before job ids: no id, the destination never read as one. */
        CHECK(RPARSE("5 6\nab\n", sizeof dest) == 1 && xpid == 5 && hpid == 6 && !rid[0] && strcmp(dest, "ab") == 0);
        CHECK(RPARSE("garbage", sizeof dest) == 0);
        CHECK(RPARSE("12 x\n/a\n", sizeof dest) == 0);
        CHECK(RPARSE("1 2 not-hex\n/a\n", sizeof dest) == 0);
        CHECK(RPARSE("1 2\n/a/very/long/destination\n", 8) == 0);
#undef RPARSE
    }

    CHECK(strcmp(cc_image_format("/data/a.b/Game.ExFAT"), "exfat") == 0);
    CHECK(strcmp(cc_image_format("/data/g.ffpkg"), "ffpkg") == 0 && strcmp(cc_image_format("/data/g.ffpfs"), "ffpfs") == 0);
    CHECK(!cc_image_format("/data/g.ffpfsc") && !cc_image_format("/data/g.pkg") && !cc_image_format("/data/a.exfat/g"));
    CHECK(!cc_image_format("/data/g.") && !cc_image_format("/data/exfat"));
    CHECK(cc_format_ok("folder") && cc_format_ok("ffpfs") && !cc_format_ok("pkg") && !cc_format_ok(""));
    CHECK(cc_path_within("/data/g", "/data/g") && cc_path_within("/data/g", "/data/g/out") && !cc_path_within("/data/g", "/data/g2"));
    /* A source a build of dest would clear. */
    CHECK(cc_build_reads_own_leftovers("/data/games/G.partial", "/data/games/G"));
    CHECK(cc_build_reads_own_leftovers("/data/games/g.PARTIAL", "/data/games/G")); /* exFAT: one name */
    CHECK(cc_build_reads_own_leftovers("/data/games/G.partial/PPSA01234", "/data/games/G"));
    CHECK(cc_build_reads_own_leftovers("/data/games/G.exfat.partial.resume", "/data/games/G.exfat"));
    CHECK(cc_build_reads_own_leftovers("/data/games/G.exfat.partial.stamp", "/data/games/G.exfat"));
    CHECK(cc_build_reads_own_leftovers("/data/games", "/data/games/G"));
    CHECK(!cc_build_reads_own_leftovers("/data/games/G.partial2", "/data/games/G"));
    CHECK(!cc_build_reads_own_leftovers("/data/games/G", "/data/out/G.exfat"));
    CHECK(!cc_build_reads_own_leftovers("/data/games/G.partia", "/data/games/G"));

    /* A left-behind request: the extractor's rule. */
    CHECK(!console_convert_request_stale(1000, 1000 + CONVERT_REQUEST_STALE_S - 1));
    CHECK(console_convert_request_stale(1000, 1000 + CONVERT_REQUEST_STALE_S));
    CHECK(console_convert_request_stale(1000, 999));
}

static void test_op(void) {
    char args[2048], game[300], image[300], dest[300], partial[320], other[300];
    ava1_op_ctx_t *c = malloc(sizeof *c);
    int st;

    snprintf(game, sizeof game, "%s/GAME", g_root);
    snprintf(image, sizeof image, "%s/game.exfat", g_root);
    snprintf(dest, sizeof dest, "%s/out.ffpkg", g_root);
    snprintf(partial, sizeof partial, "%s.partial", dest);
    snprintf(other, sizeof other, "%s/notes.txt", g_root);
    mkdir(game, 0700);
    put_file(image, "x");
    put_file(other, "x");

    /* Refused before anything is started. */
    CHECK(run_op(FAKE_NEVER, "{}", c) == AVA1_ERR_PROTOCOL && strcmp(c->message, "convert_missing_path") == 0);
    snprintf(args, sizeof args, "{\"path\":\"%s\",\"action\":\"mount\"}", game);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PROTOCOL && strcmp(c->message, "convert_bad_action") == 0);
    CHECK(run_op(FAKE_NEVER, "{\"path\":\"/etc/passwd\"}", c) == AVA1_ERR_PATH && strcmp(c->message, "convert_path_not_allowed") == 0);
    snprintf(args, sizeof args, "{\"path\":\"%s/../GAME\"}", game);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_path_not_allowed") == 0);
    snprintf(args, sizeof args, "{\"path\":\"%s/refused\"}", g_root);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_path_not_allowed") == 0);
    snprintf(args, sizeof args, "{\"path\":\"%s/missing\"}", g_root);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_open_failed") == 0);
    snprintf(args, sizeof args, "{\"path\":\"%s\"}", other);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_unsupported_source") == 0);
#define BUILD_REFUSED(DEST, FORMAT, STATUS, CAUSE)                                                                    \
    do {                                                                                                              \
        snprintf(args, sizeof args, "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"%s\"}", game, \
                 DEST, FORMAT);                                                                                       \
        st = run_op(FAKE_NEVER, args, c);                                                                             \
        CHECK(st == (int)(STATUS) && strcmp(c->message, CAUSE) == 0);                                                 \
        CHECK(!g_request[0]); /* no converter was started */                                                          \
    } while (0)
    BUILD_REFUSED("", "exfat", AVA1_ERR_PROTOCOL, "convert_missing_dest");
    BUILD_REFUSED(dest, "pkg", AVA1_ERR_PROTOCOL, "convert_bad_format");
    BUILD_REFUSED("/etc/out.ffpkg", "ffpkg", AVA1_ERR_PATH, "convert_path_not_allowed");
    BUILD_REFUSED(dest, "exfat", AVA1_ERR_PATH, "convert_bad_dest");  /* the name says another format */
    BUILD_REFUSED(image, "exfat", AVA1_ERR_EXISTS, "convert_destination_exists");
    {
        char inside[320], nodir[320], folder_named_image[320];
        snprintf(inside, sizeof inside, "%s/copy", game);
        snprintf(nodir, sizeof nodir, "%s/nowhere/out.ffpkg", g_root);
        snprintf(folder_named_image, sizeof folder_named_image, "%s/out.exfat", g_root);
        BUILD_REFUSED(inside, "folder", AVA1_ERR_PATH, "convert_bad_dest");
        BUILD_REFUSED(nodir, "ffpkg", AVA1_ERR_PATH, "convert_bad_dest");
        BUILD_REFUSED(folder_named_image, "folder", AVA1_ERR_PATH, "convert_bad_dest");
    }
    {
        /* A source named as the build's unfinished output: refused, and never removed. */
        char src_partial[320], src_eboot[340], pdest[320];
        snprintf(src_partial, sizeof src_partial, "%s/P.partial", g_root);
        snprintf(src_eboot, sizeof src_eboot, "%s/eboot.bin", src_partial);
        snprintf(pdest, sizeof pdest, "%s/P", g_root);
        mkdir(src_partial, 0700);
        put_file(src_eboot, "x");
        snprintf(args, sizeof args, "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"folder\"}",
                 src_partial, pdest);
        CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_bad_dest") == 0 && !g_request[0]);
        CHECK(exists(src_eboot));
    }
    /* A zstd level outside zstd's range. */
    snprintf(args, sizeof args,
             "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"ffpkg\",\"zstd\":true,\"zstd_level\":23}", game,
             dest);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PROTOCOL && strcmp(c->message, "convert_bad_zstd_level") == 0 && !g_request[0]);

    /* An inspection: the request names it, the status is followed past a half-written and a
     * garbage file, and the converter's answer is the job's result, byte for byte. */
    snprintf(args, sizeof args, "{\"path\":\"%s\"}", image);
    g_ticks = 0;
    st = run_op(FAKE_INSPECT, args, c);
    CHECK(st == AVA1_STATUS_OK && !c->message[0]);
    /* No zstd runtime on the console yet: the answer says so. */
    CHECK(strcmp(c->result,
                 "{\"path\":\"/x/a \\\"b\\\" }\",\"files\":3,\"warnings\":[\"no {eboot}\"],\"zstd_runtime\":null}") == 0);
    {
        char got[400];
        CHECK(webui_json_str(g_request, strlen(g_request), "op", got, sizeof got) == 1 && strcmp(got, "inspect") == 0);
        CHECK(webui_json_str(g_request, strlen(g_request), "path", got, sizeof got) == 1 && strcmp(got, image) == 0);
        CHECK(webui_json_str(g_request, strlen(g_request), "dest", got, sizeof got) == 0);
    }
    CHECK(g_ticks == 0); /* the idle timer is only held off for a build */
    {
        /* Nothing of the job is left in the folder. */
        char p[400];
        path_in(p, sizeof p, "status.json");
        CHECK(!exists(p));
        path_in(p, sizeof p, "job.rec");
        CHECK(!exists(p));
        path_in(p, sizeof p, "request.json");
        CHECK(!exists(p));
    }

    /* A zstd build without the runtime is refused before a converter starts; one without zstd
     * is not held up by it. */
    snprintf(args, sizeof args, "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"ffpkg\",\"zstd\":true}",
             game, dest);
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_PATH && strcmp(c->message, "convert_no_zstd_runtime") == 0 &&
          !g_request[0]);
    snprintf(args, sizeof args, "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"ffpkg\"}", game, dest);
    CHECK(run_op(FAKE_FAILS, args, c) == AVA1_ERR_IO && strcmp(c->message, "convert_build_failed") == 0);
    {
        char got[400];
        CHECK(webui_json_str(g_request, strlen(g_request), "zstd_runtime", got, sizeof got) == 0);
    }
    /* Runtimes as the desktop app leaves them, and folders that do not count: one with no file
     * (or only the temporary one it writes first), a name that is not a runtime's. */
    put_runtime("0.3.4", "libSceAmpr.sprx");
    put_runtime("0.4.0", "libSceAmpr.sprx");
    put_runtime("0.4.1", "libSceAmpr.sprx");
    put_runtime("0.10.0", NULL);
    put_runtime("0.9.0", "libSceAmpr.sprx.part");
    {
        char p[400];
        snprintf(p, sizeof p, "%s/notes", g_rtdir);
        mkdir(p, 0700);
    }
    {
        cc_runtime_t rt;
        char want[400];
        snprintf(want, sizeof want, "%s/ampr-emu-ng-0.4.1/libSceAmpr.sprx", g_rtdir);
        CHECK(console_convert_find_runtime(&rt) == 1 && strcmp(rt.version, "0.4.1") == 0 && strcmp(rt.path, want) == 0);
    }
    snprintf(args, sizeof args, "{\"path\":\"%s\"}", image);
    CHECK(run_op(FAKE_INSPECT, args, c) == AVA1_STATUS_OK);
    {
        char got[64];
        CHECK(webui_json_str(c->result, c->result_len, "zstd_runtime", got, sizeof got) == 1 && strcmp(got, "0.4.1") == 0);
    }

    /* An inspection may run beside an install or an extraction; a build may not. */
    g_installing = 1;
    CHECK(run_op(FAKE_INSPECT, args, c) == AVA1_STATUS_OK);
    snprintf(args, sizeof args,
             "{\"path\":\"%s\",\"action\":\"build\",\"dest\":\"%s\",\"format\":\"ffpkg\",\"zstd\":true,\"zstd_level\":19,\"verify\":\"full\"}",
             game, dest);
    g_building_when_installing_read = -1;
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_BUSY && strcmp(c->message, "convert_busy") == 0 && !g_request[0]);
    /* The build's own flag was up before the install's was read (no window where neither sees
     * the other), and is down again once it backed out. */
    CHECK(g_building_when_installing_read == 1);
    CHECK(!console_convert_building());
    g_installing = 0;
    g_extracting = 1;
    g_building_when_extracting_read = -1;
    CHECK(run_op(FAKE_NEVER, args, c) == AVA1_ERR_BUSY && !g_request[0]);
    CHECK(g_building_when_extracting_read == 1);
    g_extracting = 0;
    CHECK(!console_convert_building());

    /* A build that fails: the converter's cause is the job's message. */
    st = run_op(FAKE_FAILS, args, c);
    CHECK(st == AVA1_ERR_IO && strcmp(c->message, "convert_build_failed") == 0);
    {
        char got[400];
        int z = 0;
        CHECK(webui_json_str(g_request, strlen(g_request), "op", got, sizeof got) == 1 && strcmp(got, "build") == 0);
        CHECK(webui_json_str(g_request, strlen(g_request), "dest", got, sizeof got) == 1 && strcmp(got, dest) == 0);
        CHECK(webui_json_str(g_request, strlen(g_request), "format", got, sizeof got) == 1 && strcmp(got, "ffpkg") == 0);
        CHECK(webui_json_str(g_request, strlen(g_request), "verify", got, sizeof got) == 1 && strcmp(got, "full") == 0);
        CHECK(webui_json_bool(g_request, strlen(g_request), "zstd", &z) == 1 && z == 1);
        {
            uint64_t lv = 0;
            CHECK(webui_json_u64(g_request, strlen(g_request), "zstd_level", &lv) == 1 && lv == 19);
        }
        {
            /* The newest runtime's file goes with it. */
            char want[400];
            snprintf(want, sizeof want, "%s/ampr-emu-ng-0.4.1/libSceAmpr.sprx", g_rtdir);
            CHECK(webui_json_str(g_request, strlen(g_request), "zstd_runtime", got, sizeof got) == 1 &&
                  strcmp(got, want) == 0);
        }
    }

    /* Progress, then Stop: the cancel file appears, the converter says it stopped. */
    g_ticks = 0;
    st = run_op(FAKE_BUILD_UNTIL_CANCEL, args, c);
    CHECK(st == AVA1_ERR_CANCELLED && strcmp(c->message, "convert_cancelled") == 0);
    /* The final status carries no byte counts: the total the job showed stays. The helper polls
     * slower than the stand-in writes, so it may not see every step. */
    CHECK(c->total == 1000000 && c->done >= 1000);
    CHECK(g_ticks >= 1);
    {
        char p[400];
        path_in(p, sizeof p, "cancel");
        CHECK(!exists(p)); /* removed with the rest of the job's files */
    }

    /* Stages count on top of each other: done never passes total, and both end at the sum. */
    st = run_op(FAKE_STAGES, args, c);
    CHECK(st == 0);
    CHECK(c->total == 330 && c->done == 330);

    /* A converter that ignores Stop is killed, and what it was writing is removed. */
    put_file(partial, "half an image");
    st = run_op(FAKE_HANG, args, c);
    CHECK(st == AVA1_ERR_CANCELLED && strcmp(c->message, "convert_cancelled") == 0);
    CHECK(g_child > 0 && waitpid(g_child, &st, 0) == g_child && WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL);
    CHECK(!exists(partial));

    /* One that died on its own: said so, and its output removed. */
    put_file(partial, "half an image");
    st = run_op(FAKE_DIED, args, c);
    CHECK(st == AVA1_ERR_IO && strcmp(c->message, "convert_died") == 0);
    CHECK(!exists(partial));

    /* A status naming this process (the helper's own pid) or init: never signalled, read as a
     * converter that is not running. Were either taken for the converter, Stop would SIGKILL it
     * (this test). Huge values never get here: cc_status_parse reads them as none (test_wire). */
    {
        static const long long bad[] = {0, 1, 2147483648LL};
        size_t k;
        g_fake_pid = (long long)getpid();
        st = run_op(FAKE_PID, args, c);
        CHECK(st == AVA1_ERR_CANCELLED && strcmp(c->message, "convert_cancelled") == 0);
        for (k = 0; k < sizeof bad / sizeof *bad; k++) {
            g_fake_pid = bad[k];
            st = run_op(FAKE_PID, args, c);
            CHECK(st == AVA1_ERR_CANCELLED);
        }
    }

    /* One job across helpers: another helper (this test's parent stands in for it) holds the
     * record, its converter not reporting yet. Busy, its request and record left alone. */
    {
        char rec[400], req[400], text[600], got[600];
        FILE *f;
        size_t n;
        path_in(rec, sizeof rec, "job.rec");
        path_in(req, sizeof req, "request.json");
        cc_record_format(text, sizeof text, 0, (long)getppid(), "feed", "/elsewhere/other.exfat");
        put_file(rec, text);
        put_file(req, "{\"id\":\"feed\",\"op\":\"inspect\",\"path\":\"/x\"}\n");
        st = run_op(FAKE_NEVER, args, c);
        CHECK(st == AVA1_ERR_BUSY && strcmp(c->message, "convert_busy") == 0 && !g_request[0]);
        f = fopen(rec, "r");
        n = f ? fread(got, 1, sizeof got - 1, f) : 0;
        if (f) fclose(f);
        got[n] = 0;
        CHECK(strcmp(got, text) == 0);
        CHECK(exists(req));
        unlink(req);

        /* Its helper gone (a dead pid), and no converter seen: the record is cleared, the job runs. */
        {
            pid_t dead = fork();
            if (dead == 0) _exit(0);
            waitpid(dead, NULL, 0);
            cc_record_format(text, sizeof text, 0, (long)dead, "feed", "");
            put_file(rec, text);
        }
        st = run_op(FAKE_FAILS, args, c);
        CHECK(st == AVA1_ERR_IO && strcmp(c->message, "convert_build_failed") == 0);
        CHECK(!exists(rec));

        /* Another job took the folder while this one ran: this one's clean-up leaves the other's
         * record (and the status it may own) in place. */
        st = run_op(FAKE_STOLEN, args, c);
        CHECK(st == AVA1_ERR_IO);
        f = fopen(rec, "r");
        n = f ? fread(got, 1, sizeof got - 1, f) : 0;
        if (f) fclose(f);
        got[n] = 0;
        CHECK(strstr(got, " feed\n") != NULL);
        path_in(text, sizeof text, "status.json");
        CHECK(exists(text));
        unlink(rec);
        unlink(text);
    }

    /* One that never reports. It may still start late: the cancel file is left for it (so it
     * stops before writing), the request is gone, and nothing beside dest is removed, since no
     * converter was ever seen to have died. */
    put_file(partial, "a late converter's, or an earlier run's");
    st = run_op(FAKE_NEVER, args, c);
    CHECK(st == AVA1_ERR_IO && strcmp(c->message, "convert_not_started") == 0);
    CHECK(!exists(dest)); /* the destination itself is never touched by the helper */
    CHECK(exists(partial));
    {
        char p[400];
        path_in(p, sizeof p, "cancel");
        CHECK(exists(p));
        path_in(p, sizeof p, "request.json");
        CHECK(!exists(p));
        path_in(p, sizeof p, "job.rec");
        CHECK(!exists(p));
        /* The next job removes it before its converter starts, and runs. */
        st = run_op(FAKE_FAILS, args, c);
        CHECK(st == AVA1_ERR_IO && strcmp(c->message, "convert_build_failed") == 0);
        path_in(p, sizeof p, "cancel");
        CHECK(!exists(p));
    }
    unlink(partial);
    free(c);
}

static void test_sweep(void) {
    char rec[400], req[400], dest[300], partial[320], resume[340], folder[340], inner[380], text[600];
    struct timeval old[2] = {{1000, 0}, {1000, 0}};
    pid_t dead = fork();
    if (dead == 0) _exit(0);
    waitpid(dead, NULL, 0);
    path_in(rec, sizeof rec, "job.rec");
    path_in(req, sizeof req, "request.json");
    snprintf(dest, sizeof dest, "%s/swept.exfat", g_root);
    snprintf(partial, sizeof partial, "%s.partial", dest);
    snprintf(resume, sizeof resume, "%s.partial.resume", dest);

    /* The helper that wrote the record still runs (this process stands in for another helper's
     * pid by using its parent): nothing is touched. */
    put_file(partial, "half");
    cc_record_format(text, sizeof text, (unsigned long long)dead, (long)getppid(), "ab", dest);
    put_file(rec, text);
    console_convert_sweep();
    CHECK(exists(partial) && exists(rec));

    /* Both gone: the unfinished output and its side files go, the finished name is not made up. */
    put_file(resume, "{}");
    cc_record_format(text, sizeof text, (unsigned long long)dead, (long)dead, "ab", dest);
    put_file(rec, text);
    console_convert_sweep();
    CHECK(!exists(partial) && !exists(resume) && !exists(rec) && !exists(dest));

    /* A folder output's unfinished folder. */
    snprintf(dest, sizeof dest, "%s/SWEPT", g_root);
    snprintf(folder, sizeof folder, "%s.partial", dest);
    snprintf(inner, sizeof inner, "%s/eboot.bin", folder);
    mkdir(folder, 0700);
    put_file(inner, "x");
    cc_record_format(text, sizeof text, (unsigned long long)dead, (long)dead, "ab", dest);
    put_file(rec, text);
    console_convert_sweep();
    CHECK(!exists(folder) && !exists(rec));

    /* A helper that died before its converter ever reported (pid 0 in the record): that converter
     * may still start, so it is told to stop and its request goes, but nothing beside dest is
     * removed. Another job's request stays. */
    {
        char cancel[400];
        path_in(cancel, sizeof cancel, "cancel");
        unlink(cancel);
        snprintf(dest, sizeof dest, "%s/late.exfat", g_root);
        snprintf(partial, sizeof partial, "%s.partial", dest);
        put_file(partial, "x");
        put_file(req, "{\"id\":\"ab\",\"op\":\"build\"}");
        cc_record_format(text, sizeof text, 0, (long)dead, "ab", dest);
        put_file(rec, text);
        console_convert_sweep();
        CHECK(!exists(rec) && exists(cancel) && !exists(req) && exists(partial));
        put_file(req, "{\"id\":\"cd\",\"op\":\"build\"}");
        put_file(rec, text);
        console_convert_sweep();
        CHECK(!exists(rec) && exists(req));
        unlink(req);
        unlink(cancel);
        unlink(partial);
    }

    /* A destination outside the allowed roots is never acted on; an inspection has none. */
    put_file(rec, "1 1\n/etc/hosts\n");
    console_convert_sweep();
    CHECK(!exists(rec) && exists("/etc/hosts"));
    cc_record_format(text, sizeof text, (unsigned long long)dead, (long)dead, "ab", "");
    put_file(rec, text);
    console_convert_sweep();
    CHECK(!exists(rec));
    /* Not a record: one may be being written this moment (created, then written), so only an
     * old one goes. */
    put_file(rec, "not a record");
    console_convert_sweep();
    CHECK(exists(rec));
    utimes(rec, old);
    console_convert_sweep();
    CHECK(!exists(rec));
    put_file(rec, "");
    console_convert_sweep();
    CHECK(exists(rec));
    utimes(rec, old);
    console_convert_sweep();
    CHECK(!exists(rec));

    /* A request no converter read: kept while one may still be starting, removed once old. */
    put_file(req, "{}");
    console_convert_sweep();
    CHECK(exists(req));
    utimes(req, old);
    console_convert_sweep();
    CHECK(!exists(req));
}

int main(void) {
    char tmpl[] = "/tmp/ps5upload-convert-selftest-XXXXXX", cmd[400];
    if (!mkdtemp(tmpl)) {
        perror("mkdtemp");
        return 1;
    }
    snprintf(g_root, sizeof g_root, "%s", tmpl);
    snprintf(g_dir, sizeof g_dir, "%s/convert", g_root);
    mkdir(g_dir, 0700);
    console_convert_test_dir = g_dir;
    console_convert_test_launch = fake_launch;
    snprintf(g_rtdir, sizeof g_rtdir, "%s/runtime", g_root);
    console_convert_test_runtime_dir = g_rtdir; /* made by test_op, once it has tested without one */

    g_alive = fork();
    if (g_alive == 0) {
        for (;;) pause();
    }
    if (g_alive < 0) {
        perror("fork");
        return 1;
    }

    test_wire();
    test_op();
    test_sweep();

    kill(g_alive, SIGKILL);
    waitpid(g_alive, NULL, 0);

    snprintf(cmd, sizeof cmd, "rm -r '%s'", g_root);
    if (system(cmd) != 0) fprintf(stderr, "console_convert_selftest: could not remove %s\n", g_root);
    if (failures) {
        fprintf(stderr, "console_convert_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("console_convert_selftest: ok\n");
    return 0;
}
