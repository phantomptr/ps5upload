/* Host-side tests for console_tasks.c, the apps' view of what the console page started: only noted
 * jobs are listed, progress follows the job list, how a job ended outlives the job table (a cancelled
 * copy leaves it at once), slots are reused, and the JSON is the snake_case the engine reads.
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_gen.h"
#include "console_tasks.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static uint8_t res[4096];

/* A job.list reply of up to 4 entries. */
static size_t list_of(const ava1_job_entry_t *e, unsigned n) {
    uint8_t blob[1024];
    ava1_w_t bw, w;
    ava1_job_list_result_t r;
    unsigned i;
    ava1_w_init(&bw, blob, sizeof blob);
    for (i = 0; i < n; i++) (void)ava1_job_entry_append(&bw, &e[i]);
    memset(&r, 0, sizeof r);
    r.jobs = blob;
    r.jobs_len = (uint32_t)bw.len;
    r.jobs_count = n;
    ava1_w_init(&w, res, sizeof res);
    CHECK(ava1_job_list_result_encode(&r, &w) == 0);
    return w.len;
}

static ava1_job_entry_t job(uint8_t tag, uint8_t state, uint64_t done, uint64_t total) {
    ava1_job_entry_t e;
    memset(&e, 0, sizeof e);
    memset(e.job_id, tag, 16);
    e.state = state;
    e.bytes_done = done;
    e.bytes_total = total;
    e.files_done = 1;
    e.files_total = 2;
    return e;
}

static void id_of(uint8_t tag, uint8_t id[16]) { memset(id, tag, 16); }

static char json[64 * 1024];

static void read_json(uint64_t now) { CHECK(ctasks_json(json, sizeof json, now) >= 0); }

static void test_only_noted_jobs_and_keys(void) {
    uint8_t a[16];
    ava1_job_entry_t e[2];
    size_t n;
    ctasks_reset();
    id_of(0xAA, a);
    ctasks_started(a, "copy", "/data/games/My \"Game\".iso", 1000);
    e[0] = job(0xAA, 0, 50, 100);
    e[1] = job(0xBB, 0, 7, 9); /* a paired PC's upload */
    n = list_of(e, 2);
    CHECK(ctasks_observe(res, n, 2000) == 0);
    read_json(2000);
    CHECK(strstr(json, "\"id\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"") != NULL);
    CHECK(strstr(json, "bbbbbbbb") == NULL);
    CHECK(strstr(json, "\"kind\":\"copy\"") != NULL);
    CHECK(strstr(json, "\"state\":\"running\"") != NULL);
    CHECK(strstr(json, "\"name\":\"My \\\"Game\\\".iso\"") != NULL);
    CHECK(strstr(json, "\"files_done\":1,\"files_total\":2,\"bytes_done\":50,\"bytes_total\":100,\"age_s\":0") != NULL);
    {
        uint8_t b[16];
        id_of(0xBB, b);
        CHECK(ctasks_known(a));
        CHECK(!ctasks_known(b));
    }
}

static void test_how_it_ended_is_kept(void) {
    uint8_t a[16], b[16], c[16], d[16];
    ava1_job_entry_t e[3];
    size_t n;
    ctasks_reset();
    id_of(1, a);
    id_of(2, b);
    id_of(3, c);
    id_of(4, d);
    ctasks_started(a, "copy", "/data/a", 0);
    ctasks_started(b, "delete", "/data/b/", 0);
    ctasks_started(c, "move", "/data/c", 0);
    ctasks_started(d, "verify", "/data/d", 0);
    /* a ends well, b fails, c was cancelled and left the list at once, d was stopped and ended failed */
    ctasks_cancel_requested(c);
    ctasks_cancel_requested(d);
    e[0] = job(1, 1, 10, 10);
    e[1] = job(2, 2, 1, 10);
    e[2] = job(4, 2, 1, 10);
    n = list_of(e, 3);
    CHECK(ctasks_observe(res, n, 10000) == 0);
    /* a later listing without them changes nothing: the job table forgot them, we did not */
    n = list_of(e, 0);
    CHECK(ctasks_observe(res, n, 11000) == 0);
    read_json(14000);
    CHECK(strstr(json, "\"name\":\"a\",\"files_done\":1,\"files_total\":2,\"bytes_done\":10,\"bytes_total\":10,\"age_s\":4") != NULL);
    CHECK(strstr(json, "\"kind\":\"copy\",\"state\":\"done\"") != NULL);
    CHECK(strstr(json, "\"kind\":\"delete\",\"state\":\"failed\",\"name\":\"b\"") != NULL);
    CHECK(strstr(json, "\"kind\":\"move\",\"state\":\"cancelled\"") != NULL);
    CHECK(strstr(json, "\"kind\":\"verify\",\"state\":\"cancelled\"") != NULL);
    /* out of the window, not listed any more */
    read_json(10000 + CTASKS_KEEP_MS + 2000);
    CHECK(json[0] == '\0');
}

/* Asked to stop, the stop failed, and it fails by itself later: not "cancelled". An ended one says so. */
static void test_a_stop_that_did_not_go_through(void) {
    uint8_t a[16], b[16];
    ava1_job_entry_t e[2];
    size_t n;
    ctasks_reset();
    id_of(1, a);
    id_of(2, b);
    ctasks_started(a, "copy", "/data/a", 0);
    ctasks_started(b, "copy", "/data/b", 0);
    CHECK(ctasks_cancel_requested(a) == 1);
    ctasks_cancel_failed(a);
    e[0] = job(1, 2, 1, 10);
    e[1] = job(2, 1, 10, 10);
    n = list_of(e, 2);
    CHECK(ctasks_observe(res, n, 1000) == 0);
    read_json(1000);
    CHECK(strstr(json, "\"state\":\"failed\"") != NULL);
    CHECK(strstr(json, "\"state\":\"cancelled\"") == NULL);
    CHECK(ctasks_cancel_requested(a) == 0);
    CHECK(ctasks_cancel_requested(b) == 0);
}

static void test_vanished_and_too_new(void) {
    uint8_t a[16];
    size_t n;
    ctasks_reset();
    id_of(9, a);
    ctasks_started(a, "copy", "/data/x", 5000);
    n = list_of(NULL, 0);
    /* the listing may have been taken just before the job existed */
    CHECK(ctasks_observe(res, n, 5500) == 0);
    read_json(5500);
    CHECK(strstr(json, "\"state\":\"running\"") != NULL);
    /* later: gone, and nobody saw how it ended */
    CHECK(ctasks_observe(res, n, 5000 + CTASKS_NEW_MS + 1) == 0);
    read_json(5000 + CTASKS_NEW_MS + 1);
    CHECK(strstr(json, "\"state\":\"gone\"") != NULL);
    CHECK(ctasks_observe((const uint8_t *)"\xff", 1, 9000) == -1);
}

static void test_slots_and_long_names(void) {
    char longname[400];
    unsigned i;
    uint8_t id[16];
    ctasks_reset();
    /* a name cut inside a 3-byte character keeps whole characters */
    memset(longname, 'x', sizeof longname);
    longname[0] = '/';
    for (i = 1; i + 3 < sizeof longname; i += 3) memcpy(longname + i, "\xE4\xB8\xAD", 3);
    longname[i] = '\0';
    id_of(0xC0, id);
    ctasks_started(id, "copy", longname, 0);
    read_json(0);
    {
        const char *q = strstr(json, "\"name\":\"") + 8;
        const char *end = strchr(q, '"');
        CHECK(((size_t)(end - q)) % 3 == 0);
        CHECK(end - q > 100);
    }
    /* all full and all running: a new one is not noted, nothing else is lost */
    ctasks_reset();
    for (i = 0; i < CTASKS_SLOTS; i++) {
        memset(id, 0, 16);
        id[0] = (uint8_t)(i + 1);
        ctasks_started(id, "copy", "/f", 0);
    }
    memset(id, 0, 16);
    id[0] = 0xEE;
    ctasks_started(id, "copy", "/f", 0);
    CHECK(!ctasks_known(id));
    /* one ended: its slot goes first */
    {
        ava1_job_entry_t e[1];
        size_t n;
        uint8_t first[16];
        memset(first, 0, 16);
        first[0] = 1;
        e[0] = job(0, 1, 1, 1);
        memcpy(e[0].job_id, first, 16);
        n = list_of(e, 1);
        (void)ctasks_observe(res, n, 1); /* too new for the others to count as gone */
        ctasks_started(id, "copy", "/f", 2);
        CHECK(ctasks_known(id));
        CHECK(!ctasks_known(first));
    }
    /* a buffer too small answers -1 and an empty string, never half an entry */
    CHECK(ctasks_json(json, 16, 3) == -1);
    CHECK(json[0] == '\0');
}

int main(void) {
    test_only_noted_jobs_and_keys();
    test_how_it_ended_is_kept();
    test_a_stop_that_did_not_go_through();
    test_vanished_and_too_new();
    test_slots_and_long_names();
    if (failures) {
        fprintf(stderr, "console_tasks: %d failed\n", failures);
        return 1;
    }
    printf("console_tasks: ok\n");
    return 0;
}
