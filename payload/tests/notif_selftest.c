/* Host tests for the notification store (notif.c), a real file in a fresh temp folder: what is saved
 * comes back exactly, whatever the message holds; the sequence never rewinds; a truncated or damaged
 * store keeps what it can and never runs past a buffer. Build: see the root Makefile test-payload. */
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static char g_store[64], g_store_tmp[80];
#define NOTIF_STORE_PATH g_store
#define NOTIF_STORE_TMP g_store_tmp
#include "../src/notif.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- what notif.c takes from the rest of the helper ---- */

pthread_mutex_t sony_api_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_toasts;
static char g_shown[256];
int sceNotificationSend(int32_t type, const char *msg) {
    (void)type;
    snprintf(g_shown, sizeof g_shown, "%s", msg);
    g_toasts++;
    return 0;
}

/* ---- helpers ---- */

/* The helper starting again: an empty ring, then whatever the store holds. */
static void restart(void) {
    memset(s_ring, 0, sizeof(s_ring));
    s_seq_counter = 0;
    s_write_idx = 0;
    s_count = 0;
    notif_init();
}

static void wipe(void) {
    unlink(g_store);
    unlink(g_store_tmp);
    restart();
}

static void put_store(const char *text, size_t n) {
    FILE *f = fopen(g_store, "wb");
    if (!f) abort();
    if (n && fwrite(text, 1, n, f) != n) abort();
    fclose(f);
}

static char *read_store(size_t *out_len) {
    FILE *f = fopen(g_store, "rb");
    if (!f) abort();
    char *buf = malloc(256 * 1024);
    if (!buf) abort();
    size_t n = fread(buf, 1, 256 * 1024 - 1, f);
    fclose(f);
    buf[n] = 0;
    if (out_len) *out_len = n;
    return buf;
}

static char g_list[128 * 1024];

static const char *list_since(uint64_t since) {
    size_t written = 0;
    CHECK(notif_list(since, g_list, sizeof(g_list), &written) == 0);
    CHECK(written < sizeof(g_list) && g_list[written] == 0);
    return g_list;
}

static int count_of(const char *text, const char *needle) {
    int n = 0;
    size_t len = strlen(needle);
    for (const char *p = text; (p = strstr(p, needle)) != NULL; p += len) n++;
    return n;
}

/* ---- tests ---- */

static void test_nothing_saved(void) {
    wipe();
    CHECK(strcmp(list_since(0), "{\"notifications\":[],\"seq\":0}") == 0);
}

/* A message's bytes round-trip exactly: the length up front is what lets a newline or a digit in one
 * not read as the next entry's header. */
static void test_round_trip(void) {
    wipe();
    CHECK(notif_send("plain", NOTIF_LEVEL_INFO) == 0);
    CHECK(notif_send("line one\n7 7 0 5\nlooks like a header", NOTIF_LEVEL_WARN) == 0);
    CHECK(notif_send("\"quoted\" back\\slash\ttab", NOTIF_LEVEL_ERROR) == 0);
    CHECK(notif_send("", NOTIF_LEVEL_INFO) == 0);
    char before[8192];
    snprintf(before, sizeof(before), "%s", list_since(0));

    restart();
    CHECK(strcmp(list_since(0), before) == 0);
    CHECK(s_count == 4);
    CHECK(strstr(before, "\"level\":\"warn\"") != NULL);
    CHECK(strstr(before, "\"level\":\"error\"") != NULL);
    CHECK(strstr(before, "line one\\n7 7 0 5\\nlooks like a header") != NULL);
    CHECK(strstr(before, "\\\"quoted\\\" back\\\\slash\\ttab") != NULL);
}

/* since_seq filters, and the counter never rewinds across a restart or a clear: a client holding a
 * high watermark would go silent. */
/* A PIN is shown on the TV, but the ring and its file keep the message without it. */
static void test_kept_differs_from_shown(void) {
    wipe();
    restart();
    CHECK(notif_send_kept("Remote Play PIN: 14928144", "Remote Play PIN: (shown on the TV)", NOTIF_LEVEL_INFO) == 0);
    CHECK(strcmp(g_shown, "Remote Play PIN: 14928144") == 0);
    size_t n = 0;
    char *store = read_store(&n);
    CHECK(store && !strstr(store, "14928144") && strstr(store, "(shown on the TV)"));
    free(store);
    CHECK(!strstr(list_since(0), "14928144"));
}

static void test_sequence_never_rewinds(void) {
    wipe();
    notif_send("one", NOTIF_LEVEL_INFO);
    notif_send("two", NOTIF_LEVEL_INFO);
    CHECK(count_of(list_since(1), "\"msg\"") == 1);
    CHECK(strstr(list_since(1), "two") != NULL);

    restart();
    CHECK(s_seq_counter == 2);
    notif_send("three", NOTIF_LEVEL_INFO);
    CHECK(strstr(list_since(2), "three") != NULL);
    CHECK(strstr(list_since(2), "\"seq\":3,") != NULL);

    CHECK(notif_clear() == 3);
    CHECK(count_of(list_since(0), "\"msg\"") == 0);
    restart();
    CHECK(s_count == 0);
    CHECK(s_seq_counter == 3);
    notif_send("four", NOTIF_LEVEL_INFO);
    CHECK(strstr(list_since(0), "\"seq\":4,") != NULL);
}

/* A full ring keeps the newest 64, oldest first, also after a restart. */
static void test_ring_wraps(void) {
    wipe();
    char msg[32];
    for (int i = 1; i <= 70; i++) {
        snprintf(msg, sizeof(msg), "m%d", i);
        notif_send(msg, NOTIF_LEVEL_INFO);
    }
    for (int round = 0; round < 2; round++) {
        const char *list = list_since(0);
        CHECK(count_of(list, "\"msg\"") == NOTIF_RING_SIZE);
        CHECK(strstr(list, "\"m6\"") == NULL);
        const char *first = strstr(list, "\"m7\"");
        const char *last = strstr(list, "\"m70\"");
        CHECK(first && last && first < last);
        restart();
    }
    notif_send("after", NOTIF_LEVEL_INFO);
    CHECK(strstr(list_since(70), "after") != NULL);
    CHECK(count_of(list_since(0), "\"msg\"") == NOTIF_RING_SIZE);
}

/* The longest message is cut to fit, on send and through the store. */
static void test_long_message(void) {
    wipe();
    char big[1024];
    memset(big, 'x', sizeof(big) - 1);
    big[sizeof(big) - 1] = 0;
    notif_send(big, NOTIF_LEVEL_INFO);
    CHECK(strlen(s_ring[0].message) == NOTIF_MSG_MAX - 1);
    restart();
    CHECK(s_count == 1);
    CHECK(strlen(s_ring[0].message) == NOTIF_MSG_MAX - 1);
}

/* A list reply never runs past its buffer, however small. */
static void test_small_buffers(void) {
    wipe();
    char msg[200];
    memset(msg, '"', sizeof(msg) - 1); /* each escapes to two bytes */
    msg[sizeof(msg) - 1] = 0;
    for (int i = 0; i < 5; i++) notif_send(msg, NOTIF_LEVEL_INFO);
    for (size_t cap = 1; cap < 1200; cap += 7) {
        char *buf = malloc(cap); /* exact, so ASan sees any overrun */
        size_t written = 0;
        if (!buf) abort();
        CHECK(notif_list(0, buf, cap, &written) == 0);
        CHECK(written < cap);
        CHECK(buf[written] == 0);
        free(buf);
    }
}

/* Cut at any byte: whatever loads is a prefix of what was saved, with no crash and no garbage. */
static void test_truncated_anywhere(void) {
    wipe();
    notif_send("alpha", NOTIF_LEVEL_INFO);
    notif_send("bravo", NOTIF_LEVEL_WARN);
    notif_send("charlie", NOTIF_LEVEL_ERROR);
    size_t len = 0;
    char *whole = read_store(&len);
    for (size_t cut = 0; cut <= len; cut++) {
        put_store(whole, cut);
        restart();
        CHECK(s_count <= 3);
        if (s_count >= 1) CHECK(strncmp(s_ring[0].message, "alpha", strlen(s_ring[0].message)) == 0);
        if (s_count >= 2) CHECK(strncmp(s_ring[1].message, "bravo", strlen(s_ring[1].message)) == 0);
        if (s_count == 3) CHECK(strncmp(s_ring[2].message, "charlie", strlen(s_ring[2].message)) == 0);
        (void)list_since(0);
    }
    free(whole);
    put_store("", 0);
    restart();
    CHECK(s_count == 0);
}

static void test_damaged(void) {
    /* Not ours: ignored, not guessed at. */
    wipe();
    const char *other = "something else entirely\n1 2 0 5\nhello\n";
    put_store(other, strlen(other));
    restart();
    CHECK(s_count == 0);
    CHECK(s_seq_counter == 0);

    /* A length far past the message buffer: at most the buffer is read. */
    const char *huge = "NOTIF1 9\n5 100 0 99999999\nshort\n";
    put_store(huge, strlen(huge));
    restart();
    CHECK(s_count == 1);
    CHECK(strcmp(s_ring[0].message, "short\n") == 0);

    /* A negative length reads as a huge one: the same. */
    const char *neg = "NOTIF1 9\n5 100 0 -1\nshort\n";
    put_store(neg, strlen(neg));
    restart();
    CHECK(s_count <= 1);

    /* A level that is not one reads as info. */
    const char *lvl = "NOTIF1 9\n5 100 77 2\nhi\n";
    put_store(lvl, strlen(lvl));
    restart();
    CHECK(s_count == 1);
    CHECK(s_ring[0].level == NOTIF_LEVEL_INFO);

    /* An entry newer than the saved counter moves the counter, so the sequence still never rewinds. */
    const char *ahead = "NOTIF1 3\n40 100 0 2\nhi\n";
    put_store(ahead, strlen(ahead));
    restart();
    CHECK(s_seq_counter == 40);

    /* More entries than the ring holds: the newest stay, and nothing runs off the end. */
    char *many = malloc(64 * 1024);
    if (!many) abort();
    size_t n = (size_t)snprintf(many, 64 * 1024, "NOTIF1 0\n");
    for (int i = 1; i <= 200; i++) n += (size_t)snprintf(many + n, 64 * 1024 - n, "%d 100 0 2\nhi\n", i);
    put_store(many, n);
    free(many);
    restart();
    CHECK(s_count == NOTIF_RING_SIZE);
    CHECK(s_seq_counter == 200);
}

/* A store that cannot be replaced leaves the last good one. */
static void test_failed_write_keeps_the_store(void) {
    wipe();
    notif_send("kept", NOTIF_LEVEL_INFO);
    size_t before = 0;
    char *saved = read_store(&before);
    char real_tmp[sizeof(g_store_tmp)];
    snprintf(real_tmp, sizeof(real_tmp), "%s", g_store_tmp);
    snprintf(g_store_tmp, sizeof(g_store_tmp), "%s.missing/x", g_store);
    notif_send("lost", NOTIF_LEVEL_INFO);
    snprintf(g_store_tmp, sizeof(g_store_tmp), "%s", real_tmp);
    size_t after = 0;
    char *now = read_store(&after);
    CHECK(after == before && memcmp(saved, now, before) == 0);
    free(saved);
    free(now);
}

int main(void) {
    char root[] = "/tmp/notifXXXXXX";
    if (!mkdtemp(root)) return 1;
    snprintf(g_store, sizeof(g_store), "%s/n.dat", root);
    snprintf(g_store_tmp, sizeof(g_store_tmp), "%s/n.dat.tmp", root);

    test_nothing_saved();
    test_round_trip();
    test_sequence_never_rewinds();
    test_kept_differs_from_shown();
    test_ring_wraps();
    test_long_message();
    test_small_buffers();
    test_truncated_anywhere();
    test_damaged();
    test_failed_write_keeps_the_store();

    unlink(g_store);
    unlink(g_store_tmp);
    rmdir(root);
    if (failures) {
        fprintf(stderr, "notif_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("notif_selftest: ok\n");
    return 0;
}
