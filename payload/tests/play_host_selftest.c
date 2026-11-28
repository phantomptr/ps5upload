/* Host-side tests for Play Rooms' hosted-by marker and PS5Upload Play's one-time grants
 * (play_host.c): a room is kept per engine, replaced whole, cleared by null, gone after its time;
 * fields are bounded and cleaned; the newest room leads; names come from the device list unless
 * the engine names itself; grants are one per engine, handed out once, gone after their time, and
 * never shown in the room answer.
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "play_host.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

#define B(s) s, strlen(s)
static char g_out[PLAY_HOST_OUT_MAX];
#define HAS(s) (strstr(g_out, s) != NULL)

static uint8_t K1[32], K2[32], K3[32], K4[32], K5[32];

/* the device list: K1 is "Desk-PC" (desktop, online at 192.168.1.20, Play on 19113), K2 "Phone"
 * (offline), others unknown */
static int lookup(const uint8_t key[32], mesh_engine_t *out) {
    memset(out, 0, sizeof *out);
    if (!memcmp(key, K1, 32)) {
        snprintf(out->name, sizeof out->name, "Desk-PC");
        snprintf(out->kind, sizeof out->kind, "desktop");
        out->ip = 0x1401A8C0u; /* 192.168.1.20 in network order on a little-endian host */
        out->online = 1;
        out->play_port = 19113;
        snprintf(out->app_ws, sizeof out->app_ws, "/api/play/app");
        return 1;
    }
    if (!memcmp(key, K2, 32)) {
        snprintf(out->name, sizeof out->name, "Phone");
        snprintf(out->kind, sizeof out->kind, "phone");
        return 1;
    }
    return 0;
}

static int room(const uint8_t *k, const char *body, uint64_t now) {
    return play_room_set_json(k, B(body), now, g_out, sizeof g_out);
}

static void test_rooms(void) {
    int playing = -1;
    play_host_reset();
    play_host_set_lookup(lookup);
    CHECK(play_room_json(0, g_out, sizeof g_out) > 0 && strcmp(g_out, "{\"room\":null,\"rooms\":[]}") == 0);
    CHECK(play_room_active(0, &playing) == 0 && playing == 0);
    /* refused: not an object, no name, a bad body */
    CHECK(room(K1, "[]", 0) > 0 && HAS("bad_request"));
    CHECK(room(K1, "{\"room\":{\"visibility\":\"public\"}}", 0) > 0 && HAS("bad_request"));
    CHECK(room(K1, "{\"room\":{\"name\":\"   \"}}", 0) > 0 && HAS("bad_request"));
    CHECK(room(K1, "{\"room\":[1]}", 0) > 0 && HAS("bad_request"));
    CHECK(room(K1, "{\"room\":{\"name\":\"x\",\"players\":[{\"name\":\"a\"},}]}}", 0) > 0 && HAS("bad_request"));
    /* null or absent: nothing held */
    CHECK(room(K1, "{\"room\":null}", 0) > 0 && HAS("\"ok\":true,\"rooms\":0"));
    CHECK(room(K1, "{}", 0) > 0 && HAS("\"ok\":true,\"rooms\":0"));
    /* a full room */
    CHECK(room(K1,
               "{\"room\":{\"name\":\"Sam's room\",\"visibility\":\"public\",\"locked\":true,"
               "\"game\":{\"name\":\"ASTRO BOT\",\"title_id\":\"PPSA01234\"},\"size\":4,"
               "\"players\":[{\"name\":\"Mia\",\"ping_ms\":42,\"seat\":2,\"bars\":3,\"via\":\"browser\"},"
               "{\"name\":\"Lee\",\"ping_ms\":null,\"via\":\"ps5app\"}],\"asks\":1,\"chat\":true,\"extra\":{\"x\":1}},"
               "\"ttl_s\":60}",
               1000) > 0 &&
          HAS("\"ok\":true,\"rooms\":1"));
    CHECK(play_room_json(3500, g_out, sizeof g_out) > 0);
    CHECK(HAS("\"room\":{\"engine_id\":\"0101010101"));
    CHECK(HAS("\"hosted_from\":\"Desk-PC\",\"kind\":\"desktop\",\"online\":true,\"name\":\"Sam's room\""));
    CHECK(HAS("\"visibility\":\"public\",\"locked\":true,\"game\":{\"name\":\"ASTRO BOT\",\"title_id\":\"PPSA01234\"}"));
    CHECK(HAS("\"size\":4,\"playing\":2,\"asks\":1,\"chat\":true,\"voice\":false"));
    CHECK(HAS("{\"name\":\"Mia\",\"ping_ms\":42,\"seat\":2,\"bars\":3,\"via\":\"browser\"},{\"name\":\"Lee\",\"ping_ms\":null,\"seat\":null,\"bars\":null,\"via\":\"ps5app\"}"));
    CHECK(HAS("\"age_s\":2}"));
    CHECK(!HAS("extra"));
    CHECK(play_room_active(3500, &playing) == 1 && playing == 2);
    /* replaced whole: what is not sent again is gone */
    CHECK(room(K1, "{\"room\":{\"name\":\"Sam's room\",\"playing\":3}}", 4000) > 0);
    CHECK(play_room_json(4000, g_out, sizeof g_out) > 0 && HAS("\"visibility\":\"private\",\"locked\":false,\"game\":null,\"size\":null,\"playing\":3") &&
          HAS("\"players\":[]"));
    /* a second engine names itself; the newest leads; both are listed */
    CHECK(room(K2, "{\"room\":{\"name\":\"Kids\",\"hosted_from\":\"Living room PC\",\"visibility\":\"friends\"}}", 5000) > 0 &&
          HAS("\"rooms\":2"));
    CHECK(play_room_json(5000, g_out, sizeof g_out) > 0 && HAS("\"room\":{\"engine_id\":\"0202") &&
          HAS("\"hosted_from\":\"Living room PC\",\"kind\":\"phone\",\"online\":false") && HAS("\"rooms\":[{"));
    /* a device the list does not know: no name, no kind */
    CHECK(room(K3, "{\"room\":{\"name\":\"Other\"}}", 5500) > 0);
    CHECK(play_room_json(5500, g_out, sizeof g_out) > 0 && HAS("\"hosted_from\":\"\",\"kind\":\"\",\"online\":false"));
    CHECK(room(K4, "{\"room\":{\"name\":\"Fourth\"}}", 5600) > 0 && HAS("\"rooms\":4"));
    /* a fifth is refused; the held ones stay */
    CHECK(room(K5, "{\"room\":{\"name\":\"Fifth\"}}", 5700) > 0 && HAS("rooms_full"));
    CHECK(play_room_active(5700, NULL) == 4);
    CHECK(room(K4, "{\"room\":null}", 5800) > 0 && HAS("\"rooms\":3"));
    CHECK(room(K3, "{\"room\":null}", 5800) > 0 && HAS("\"rooms\":2"));
    /* gone after its time (K1: 90 s default from 4000; K2: from 5000) */
    CHECK(play_room_active(93999, NULL) == 2);
    CHECK(play_room_active(94000, NULL) == 1);
    CHECK(play_room_json(94500, g_out, sizeof g_out) > 0 && HAS("Kids") && !HAS("Sam's room"));
    /* the time is bounded: 1 s means 10, a day means 600 */
    CHECK(room(K1, "{\"room\":{\"name\":\"A\"},\"ttl_s\":1}", 100000) > 0);
    CHECK(play_room_active(109999, NULL) >= 1);
    {
        int n = play_room_active(110000, NULL);
        CHECK(n == 0); /* K2 (95 s) went too */
    }
    CHECK(room(K1, "{\"room\":{\"name\":\"A\"},\"ttl_s\":86400}", 200000) > 0);
    CHECK(play_room_active(799999, NULL) == 1 && play_room_active(800000, NULL) == 0);
    /* strings cleaned: control characters dropped, cut at a character boundary, spaces trimmed */
    {
        char body[1024], name[200];
        memset(name, 0, sizeof name);
        /* 95 ASCII bytes then a 3-byte character: 98 > 96, the character goes whole */
        memset(name, 'a', 95);
        strcat(name, "\xe2\x82\xac");
        snprintf(body, sizeof body, "{\"room\":{\"name\":\"  %s\",\"players\":[{\"name\":\"Ti\\u0007m\",\"via\":\"Bad Via!\"}]}}", name);
        CHECK(room(K1, body, 300000) > 0 && HAS("\"ok\":true"));
        CHECK(play_room_json(300000, g_out, sizeof g_out) > 0);
        {
            char want[128];
            memset(want, 0, sizeof want);
            strcpy(want, "\"name\":\"");
            memset(want + 8, 'a', 95);
            strcat(want, "\",\"visibility\"");
            CHECK(HAS(want));
        }
        CHECK(HAS("{\"name\":\"Tim\",\"ping_ms\":null,\"seat\":null,\"bars\":null,\"via\":\"\"}"));
    }
    /* quotes and backslashes come out escaped */
    CHECK(room(K1, "{\"room\":{\"name\":\"a \\\"b\\\" \\\\ c\",\"game\":{\"title_id\":\"bad id\"}}}", 300100) > 0);
    CHECK(play_room_json(300100, g_out, sizeof g_out) > 0 && HAS("\"name\":\"a \\\"b\\\" \\\\ c\"") && HAS("\"game\":null"));
    /* many players: eight listed, all counted */
    {
        char body[4096];
        size_t at;
        at = (size_t)snprintf(body, sizeof body, "{\"room\":{\"name\":\"Big\",\"players\":[");
        for (int i = 0; i < 11; i++) at += (size_t)snprintf(body + at, sizeof body - at, "%s{\"name\":\"P%d\",\"ping_ms\":%d}", i ? "," : "", i, 10 * i);
        snprintf(body + at, sizeof body - at, "]}}");
        CHECK(room(K1, body, 301000) > 0 && HAS("\"ok\":true"));
        CHECK(play_room_json(301000, g_out, sizeof g_out) > 0 && HAS("\"playing\":11") && HAS("\"P7\"") && !HAS("\"P8\""));
    }
    play_host_reset();
}

static void test_grants(void) {
    const char *G1 = "{\"grant\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}";
    play_host_reset();
    play_host_set_lookup(lookup);
    CHECK(play_grants_take_json(0, g_out, sizeof g_out) > 0 && strcmp(g_out, "{\"grants\":[]}") == 0);
    /* refused: not 64 hex */
    CHECK(play_grant_set_json(K1, B("{\"grant\":\"abc\"}"), 0, g_out, sizeof g_out) > 0 && HAS("bad_request"));
    CHECK(play_grant_set_json(K1, B("{\"grant\":\"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\"}"), 0, g_out, sizeof g_out) > 0 &&
          HAS("bad_request"));
    CHECK(play_grant_set_json(K1, B("{}"), 0, g_out, sizeof g_out) > 0 && HAS("bad_request"));
    /* one, default time */
    CHECK(play_grant_set_json(K1, B(G1), 1000, g_out, sizeof g_out) > 0 && HAS("\"ok\":true,\"expires_s\":300"));
    /* never in the room answer */
    CHECK(play_room_json(1000, g_out, sizeof g_out) > 0 && !HAS("aaaa"));
    /* handed out once, with where the app reaches that engine */
    CHECK(play_grants_take_json(2000, g_out, sizeof g_out) > 0);
    CHECK(HAS("{\"grants\":[{\"engine_id\":\"0101"));
    CHECK(HAS("\"name\":\"Desk-PC\",\"kind\":\"desktop\",\"addr\":\"192.168.1.20\",\"port\":19113,\"app_ws\":\"/api/play/app\","
              "\"grant\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"expires_s\":299}"));
    CHECK(play_grants_take_json(2001, g_out, sizeof g_out) > 0 && strcmp(g_out, "{\"grants\":[]}") == 0);
    /* a new one replaces the engine's old one; upper case is kept lower */
    CHECK(play_grant_set_json(K1, B("{\"grant\":\"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\",\"ttl_s\":5}"), 3000, g_out,
                              sizeof g_out) > 0 &&
          HAS("\"expires_s\":30"));
    CHECK(play_grant_set_json(K1, B("{\"grant\":\"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\",\"ttl_s\":9999}"), 3000, g_out,
                              sizeof g_out) > 0 &&
          HAS("\"expires_s\":600"));
    CHECK(play_grant_set_json(K3, B(G1), 3000, g_out, sizeof g_out) > 0);
    CHECK(play_grants_take_json(4000, g_out, sizeof g_out) > 0 && HAS("cccc") && !HAS("bbbb") && !HAS("BBBB") && HAS("aaaa"));
    /* an unknown device: no address, no port */
    CHECK(HAS("\"name\":\"\",\"kind\":\"\",\"addr\":null,\"port\":null,\"app_ws\":null"));
    /* gone after its time, unread */
    CHECK(play_grant_set_json(K1, B(G1), 10000, g_out, sizeof g_out) > 0);
    CHECK(play_grants_take_json(310000, g_out, sizeof g_out) > 0 && strcmp(g_out, "{\"grants\":[]}") == 0);
    /* revoked by the engine */
    CHECK(play_grant_set_json(K1, B(G1), 400000, g_out, sizeof g_out) > 0);
    CHECK(play_grant_set_json(K1, B("{\"grant\":null}"), 400001, g_out, sizeof g_out) > 0 && HAS("\"expires_s\":0"));
    CHECK(play_grants_take_json(400002, g_out, sizeof g_out) > 0 && strcmp(g_out, "{\"grants\":[]}") == 0);
    /* a buffer too short hands nothing out and loses nothing */
    CHECK(play_grant_set_json(K1, B(G1), 500000, g_out, sizeof g_out) > 0);
    {
        char small[64];
        CHECK(play_grants_take_json(500001, small, sizeof small) == -1);
    }
    CHECK(play_grants_take_json(500002, g_out, sizeof g_out) > 0 && HAS("aaaa"));
    play_host_reset();
}

/* The app's next screen: set by Minion, taken once, gone after a minute, only known words. */
static void test_intent(void) {
    play_host_reset();
    CHECK(play_intent_take_json(0, g_out, sizeof g_out) > 0 && !strcmp(g_out, "{\"intent\":null}"));
    CHECK(play_intent_set("delete", 0) == -1 && play_intent_set(NULL, 0) == -1);
    CHECK(play_intent_set("join", 1000) == 0);
    CHECK(play_intent_take_json(2000, g_out, sizeof g_out) > 0 && !strcmp(g_out, "{\"intent\":\"join\"}"));
    CHECK(play_intent_take_json(2001, g_out, sizeof g_out) > 0 && !strcmp(g_out, "{\"intent\":null}"));
    CHECK(play_intent_set("rooms", 1000) == 0);
    CHECK(play_intent_take_json(61000, g_out, sizeof g_out) > 0 && !strcmp(g_out, "{\"intent\":null}"));
}

int main(void) {
    memset(K1, 1, 32);
    memset(K2, 2, 32);
    memset(K3, 3, 32);
    memset(K4, 4, 32);
    memset(K5, 5, 32);
    test_rooms();
    test_grants();
    test_intent();
    if (failures) {
        fprintf(stderr, "play_host selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("play_host selftest: ok\n");
    return 0;
}
