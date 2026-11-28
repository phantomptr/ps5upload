/* console_queue (#982): order, cancel, move to top, failures that do not stop the rest, waiting
 * for the console's one install, and what a helper restart leaves. Host build. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "console_queue.h"

static int failures;
#define CHECK(expr)                                                         \
    do {                                                                    \
        if (!(expr)) {                                                      \
            fprintf(stderr, "%s:%d: CHECK(%s)\n", __FILE__, __LINE__, #expr); \
            failures++;                                                     \
        }                                                                   \
    } while (0)

static char json[256 * 1024];
static char file[256 * 1024];

static uint32_t next_id(void) {
    cq_item_t it;
    if (!cq_take_next(&it, 100, 0)) return 0;
    return it.id;
}

static void test_order_and_failures_go_on(void) {
    long a, b, c;
    cq_reset();
    a = cq_add(CQ_COPY, "smb://nas/Games/A", "/data/games/A", "A", "k:A", 10);
    b = cq_add(CQ_INSTALL, "/data/pkg/B.pkg", NULL, "B", "k:B", 11);
    c = cq_add(CQ_COPY, "smb://nas/Games/C", "/data/games/C", "C", "k:C", 12);
    CHECK(a > 0 && b > a && c > b);
    /* the same item twice is one item */
    CHECK(cq_add(CQ_COPY, "smb://nas/Games/A", "/data/games/A", "A", "k:A", 13) == a);
    /* the same source to another folder is another */
    CHECK(cq_add(CQ_COPY, "smb://nas/Games/A", "/mnt/ext0/A", "A", "k:A", 13) > c);
    CHECK(cq_add(CQ_COPY, "smb://nas/x", NULL, "x", "", 1) == -1);
    CHECK(cq_add(9, "smb://nas/x", "/d", "x", "", 1) == -1);
    CHECK((long)next_id() == a);
    /* one at a time */
    CHECK(next_id() == 0);
    cq_finish((uint32_t)a, CQ_FAILED, "smb_no_space", 20);
    /* a failure does not stop the queue */
    CHECK((long)next_id() == b);
    cq_finish((uint32_t)b, CQ_DONE, NULL, 30);
    CHECK((long)next_id() == c);
    CHECK(cq_json(json, sizeof json, 40) > 0);
    CHECK(strstr(json, "\"error\":\"smb_no_space\"") != NULL);
    /* finished first, then the running one, then the pending ones */
    {
        const char *pa = strstr(json, "\"name\":\"A\""), *pb = strstr(json, "\"name\":\"B\""), *pc = strstr(json, "\"name\":\"C\"");
        CHECK(pa && pb && pc && pa < pb && pb < pc);
    }
    CHECK(strstr(json, "\"state\":\"running\",\"step\":0") != NULL);
    CHECK(cq_clear_finished() == 2);
    CHECK(cq_json(json, sizeof json, 40) > 0);
    CHECK(strstr(json, "\"name\":\"B\"") == NULL);
}

static void test_cancel_and_top(void) {
    long a, b, c;
    char job[CQ_JOB_MAX];
    cq_reset();
    a = cq_add(CQ_COPY, "smb://s/a", "/data/a", "a", "", 1);
    b = cq_add(CQ_COPY, "smb://s/b", "/data/b", "b", "", 2);
    c = cq_add(CQ_COPY, "smb://s/c", "/data/c", "c", "", 3);
    CHECK(cq_top((uint32_t)c) == 1);
    CHECK((long)next_id() == c);
    CHECK(cq_top((uint32_t)c) == 0); /* running: not pending */
    cq_set_job((uint32_t)c, "smb:42", 0);
    /* cancel the pending one: gone from the line */
    CHECK(cq_cancel((uint32_t)a, job) == 1);
    /* cancel the running one: the caller is told which job to stop */
    CHECK(cq_cancel((uint32_t)c, job) == 2);
    CHECK(strcmp(job, "smb:42") == 0);
    /* its job ends with an error after the stop: that is a cancel, not a failure */
    cq_finish((uint32_t)c, CQ_FAILED, "io_error", 9);
    CHECK(cq_cancel((uint32_t)c, job) == -1);
    CHECK(cq_cancel(999, job) == 0);
    CHECK((long)next_id() == b);
    CHECK(cq_json(json, sizeof json, 10) > 0);
    CHECK(strstr(json, "\"name\":\"c\",\"key\":\"\",\"job\":\"\",\"error\":\"\"") != NULL);
    CHECK(strstr(json, "\"state\":\"cancelled\"") != NULL);
}

static void test_waits_for_the_console(void) {
    long a, b;
    cq_reset();
    a = cq_add(CQ_INSTALL, "/data/pkg/A.pkg", NULL, "A", "", 1);
    b = cq_add(CQ_INSTALL, "/data/pkg/B.pkg", NULL, "B", "", 2);
    CHECK((long)next_id() == a);
    /* the PC app is installing something: A waits, first in line, and holds B */
    cq_wait((uint32_t)a, "pkg_install_busy", 5000);
    {
        cq_item_t it;
        CHECK(cq_take_next(&it, 100, 4999) == 0);
        CHECK(cq_take_next(&it, 100, 5000) == 1 && (long)it.id == a);
    }
    CHECK(cq_json(json, sizeof json, 10) > 0);
    CHECK(strstr(json, "\"wait\":\"\"") != NULL);
    cq_finish((uint32_t)a, CQ_DONE, NULL, 50);
    CHECK((long)next_id() == b);
    /* a stop asked while it waited wins */
    {
        char job[CQ_JOB_MAX];
        CHECK(cq_cancel((uint32_t)b, job) == 2);
        cq_wait((uint32_t)b, "pkg_install_busy", 1);
        CHECK(next_id() == 0);
    }
}

static void test_pause(void) {
    cq_reset();
    (void)cq_add(CQ_COPY, "smb://s/a", "/data/a", "a", "", 1);
    cq_set_paused(1, "");
    CHECK(next_id() == 0);
    cq_set_paused(0, NULL);
    CHECK(next_id() != 0);
}

static void test_restart(void) {
    long a, b, c;
    int n;
    cq_reset();
    a = cq_add(CQ_COPY, "smb://s/\"quoted\" game", "/data/a", "Ünïcode \"A\"", "lib|smb://s/a", 1);
    b = cq_add(CQ_COPY, "smb://s/b", "/data/b", "b", "", 2);
    c = cq_add(CQ_COPY, "smb://s/c", "/data/c", "c", "", 3);
    CHECK(cq_top((uint32_t)c) == 1);
    CHECK((long)next_id() == c);
    CHECK(cq_dirty());
    n = cq_serialize(file, sizeof file);
    CHECK(n > 0);
    cq_saved();
    CHECK(!cq_dirty());
    /* the helper restarts with c running: a copy, so the rest goes on */
    cq_reset();
    CHECK(cq_deserialize(file, (size_t)n, 500) == 3);
    CHECK(!cq_paused());
    CHECK(cq_json(json, sizeof json, 600) > 0);
    CHECK(strstr(json, "\"error\":\"interrupted\"") != NULL);
    CHECK(strstr(json, "\"name\":\"\\u00dcn\\u00efcode \\\"A\\\"\"") != NULL || strstr(json, "\"name\":\"Ünïcode \\\"A\\\"\"") != NULL);
    CHECK(strstr(json, "\"src\":\"smb://s/\\\"quoted\\\" game\"") != NULL);
    /* the order survived: a, then b */
    CHECK((long)next_id() == a);
    cq_finish((uint32_t)a, CQ_DONE, NULL, 600);
    CHECK((long)next_id() == b);
    /* new ids never reuse an old one */
    CHECK(cq_add(CQ_COPY, "smb://s/d", "/data/d", "d", "", 700) > c);

    /* an install interrupted: the queue pauses and says why */
    cq_reset();
    a = cq_add(CQ_INSTALL, "smb://s/a.pkg", NULL, "a", "", 1);
    b = cq_add(CQ_COPY, "smb://s/b", "/data/b", "b", "", 2);
    CHECK((long)next_id() == a);
    cq_set_job((uint32_t)a, "0123456789abcdef0123456789abcdef", 1);
    n = cq_serialize(file, sizeof file);
    cq_reset();
    CHECK(cq_deserialize(file, (size_t)n, 900) == 2);
    CHECK(cq_paused());
    CHECK(cq_json(json, sizeof json, 900) > 0);
    CHECK(strstr(json, "\"paused\":true,\"reason\":\"interrupted_install\"") != NULL);
    CHECK(next_id() == 0);
    cq_set_paused(0, NULL);
    CHECK((long)next_id() == b);

    /* not a queue file */
    CHECK(cq_deserialize("garbage", 7, 1) == -1);
    CHECK(cq_deserialize("{\"v\":1}\n{broken\n", 16, 1) == 0);
}

static void test_full(void) {
    unsigned i;
    char src[64];
    cq_reset();
    for (i = 0; i < CQ_MAX; i++) {
        snprintf(src, sizeof src, "smb://s/%u", i);
        CHECK(cq_add(CQ_COPY, src, "/data/x", "x", "", i) > 0);
    }
    CHECK(cq_add(CQ_COPY, "smb://s/more", "/data/x", "x", "", 99) == 0);
    /* one finished makes room */
    {
        uint32_t id = next_id();
        cq_finish(id, CQ_DONE, NULL, 100);
    }
    CHECK(cq_add(CQ_COPY, "smb://s/more", "/data/x", "x", "", 101) > 0);
    CHECK(cq_json(json, sizeof json, 1) > 0);
    CHECK(cq_json(json, 64, 1) == -1);
    CHECK(cq_serialize(file, sizeof file) > 0);
}

int main(void) {
    test_order_and_failures_go_on();
    test_cancel_and_top();
    test_waits_for_the_console();
    test_pause();
    test_restart();
    test_full();
    if (failures) {
        fprintf(stderr, "console_queue: %d failure(s)\n", failures);
        return 1;
    }
    printf("console_queue: ok\n");
    return 0;
}
