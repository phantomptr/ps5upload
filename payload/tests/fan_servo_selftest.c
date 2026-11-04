/* Host test for fan_servo.c: the target write keeps every other profile byte (no zero-filled SET),
 * a failed GET writes nothing, the read-back is checked, the watcher writes only when the system
 * reset the target and gives up after FAN_GUARD_MAX_FAILS, an old 15 s fan_reapply.conf moves to
 * the 2 s default, a target above the console's own is refused, and the 8F09/8F02 decoding. */
#include <stdio.h>
#include <string.h>
#include "../src/fan_servo.c"

static int fails;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            fails++;                                                   \
        }                                                              \
    } while (0)

/* A fake controller: GET copies `state` out (bytes 2..27, as selected by byte 2 = 0), SET takes
 * bytes 2..27 in. Knobs make each step fail or ignore the target. */
typedef struct {
    uint8_t state[FAN_SERVO_LEN];
    int gets, sets;
    int fail_get_at; /* the nth GET (1-based) fails; 0 never */
    int fail_set;
    int ignore_target; /* SET takes everything but byte 5 */
    int touch_byte;    /* SET also changes this byte (0 = none) */
    uint8_t last_set[FAN_SERVO_LEN];
} fake_t;

static int fake_io(void *ctx, unsigned long req, uint8_t *buf) {
    fake_t *f = ctx;
    if (req == FAN_SERVO_IOCTL_GET) {
        f->gets++;
        if (f->fail_get_at && f->gets == f->fail_get_at) return -1;
        if (buf[2] != 0) return -1; /* only selector 0 in this fake */
        memcpy(buf + 2, f->state + 2, FAN_SERVO_LEN - 2);
        buf[0] = buf[1] = 0;
        return 0;
    }
    if (req == FAN_SERVO_IOCTL_SET) {
        f->sets++;
        memcpy(f->last_set, buf, FAN_SERVO_LEN);
        if (f->fail_set) return -1;
        uint8_t keep = f->state[FAN_SERVO_TARGET];
        memcpy(f->state + 2, buf + 2, FAN_SERVO_LEN - 2);
        if (f->ignore_target) f->state[FAN_SERVO_TARGET] = keep;
        if (f->touch_byte) f->state[f->touch_byte] ^= 0x5a;
        return 0;
    }
    return -1;
}

static void fake_init(fake_t *f) {
    memset(f, 0, sizeof *f);
    for (int i = 2; i < FAN_SERVO_LEN; i++) f->state[i] = (uint8_t)(0x10 + i); /* nonzero params */
    f->state[2] = 0;                                                         /* selector */
    f->state[FAN_SERVO_TARGET] = 91;
}

static void test_set_keeps_the_profile(void) {
    fake_t f;
    uint8_t before[FAN_SERVO_LEN], after[FAN_SERVO_LEN], want[FAN_SERVO_LEN];
    int others = -1;
    fake_init(&f);
    memcpy(want, f.state, sizeof want);
    want[FAN_SERVO_TARGET] = 75;
    CHECK(fan_servo_set_target(fake_io, &f, 75, before, after, &others) == FAN_SERVO_OK);
    CHECK(f.sets == 1 && f.gets == 2);
    CHECK(memcmp(f.state + 2, want + 2, FAN_SERVO_LEN - 2) == 0); /* nothing but byte 5 moved */
    CHECK(f.last_set[3] == f.state[3] && f.last_set[27] == f.state[27]); /* not a zero-filled SET */
    CHECK(before[FAN_SERVO_TARGET] == 91 && after[FAN_SERVO_TARGET] == 75);
    CHECK(others == 0);
}

static void test_failed_get_writes_nothing(void) {
    fake_t f;
    fake_init(&f);
    f.fail_get_at = 1;
    CHECK(fan_servo_set_target(fake_io, &f, 70, NULL, NULL, NULL) == FAN_SERVO_E_GET);
    CHECK(f.sets == 0 && f.state[FAN_SERVO_TARGET] == 91);
}

static void test_readback_is_checked(void) {
    fake_t f;
    int others = 0;
    fake_init(&f);
    f.ignore_target = 1;
    CHECK(fan_servo_set_target(fake_io, &f, 70, NULL, NULL, NULL) == FAN_SERVO_E_NOT_APPLIED);
    fake_init(&f);
    f.fail_set = 1;
    CHECK(fan_servo_set_target(fake_io, &f, 70, NULL, NULL, NULL) == FAN_SERVO_E_SET);
    fake_init(&f);
    f.fail_get_at = 2;
    CHECK(fan_servo_set_target(fake_io, &f, 70, NULL, NULL, NULL) == FAN_SERVO_E_VERIFY);
    /* another byte moved: still applied, but reported */
    fake_init(&f);
    f.touch_byte = 9;
    CHECK(fan_servo_set_target(fake_io, &f, 70, NULL, NULL, &others) == FAN_SERVO_OK);
    CHECK(others == 1);
}

static void test_hex(void) {
    uint8_t o[FAN_SERVO_LEN] = {0};
    char h[2 * FAN_SERVO_LEN + 1];
    o[5] = 0x5b;
    o[27] = 0xff;
    fan_servo_hex(o, h);
    CHECK(strlen(h) == 56);
    CHECK(strncmp(h, "00000000005b", 12) == 0);
    CHECK(strcmp(h + 54, "ff") == 0);
}

static void test_watcher(void) {
    fan_guard_t g = FAN_GUARD_INIT;
    CHECK(fan_watch_decide(&g, 0, 91) == FAN_WATCH_IDLE);    /* nothing pinned: no ioctl */
    CHECK(fan_watch_decide(&g, 75, 75) == FAN_WATCH_HOLD);   /* ours: no write */
    CHECK(fan_watch_decide(&g, 75, -1) == FAN_WATCH_HOLD);   /* unreadable: never write blind */
    CHECK(fan_watch_decide(&g, 75, 91) == FAN_WATCH_RESTORE); /* a game start put 91 back */
    CHECK(strcmp(fan_guard_name(&g, 0), "off") == 0);
    CHECK(strcmp(fan_guard_name(&g, 75), "ok") == 0);
    CHECK(fan_guard_last_ago(&g, 100) == -1);
    fan_guard_note(&g, 1, 100);
    CHECK(g.restores == 1 && fan_guard_last_ago(&g, 112) == 12);
    /* four failures and a success: still going */
    for (int i = 0; i < FAN_GUARD_MAX_FAILS - 1; i++) fan_guard_note(&g, 0, 200);
    CHECK(!g.failing);
    fan_guard_note(&g, 1, 210);
    CHECK(g.fails == 0 && g.restores == 2);
    /* five in a row: stop writing until the user sets a target */
    for (int i = 0; i < FAN_GUARD_MAX_FAILS; i++) fan_guard_note(&g, 0, 300);
    CHECK(g.failing);
    CHECK(fan_watch_decide(&g, 75, 91) == FAN_WATCH_IDLE);
    CHECK(strcmp(fan_guard_name(&g, 75), "failing") == 0);
    fan_guard_clear(&g);
    CHECK(!g.failing && g.restores == 2);
    CHECK(fan_watch_decide(&g, 75, 91) == FAN_WATCH_RESTORE);
}

static void test_reapply_conf(void) {
    CHECK(fan_reapply_parse(NULL, 2, 1, 300) == 2);
    CHECK(fan_reapply_parse("15\n", 2, 1, 300) == 2);    /* what the 15 s builds wrote */
    CHECK(fan_reapply_parse("v2 15\n", 2, 1, 300) == 2);
    CHECK(fan_reapply_parse("30\n", 2, 1, 300) == 30);   /* someone's own choice stays */
    CHECK(fan_reapply_parse("v3 15\n", 2, 1, 300) == 15); /* chosen after the move: kept */
    CHECK(fan_reapply_parse("v3 5\n", 2, 1, 300) == 5);
    CHECK(fan_reapply_parse("v3 0\n", 2, 1, 300) == 2);
    CHECK(fan_reapply_parse("v3 999\n", 2, 1, 300) == 2);
    CHECK(fan_reapply_parse("garbage", 2, 1, 300) == 2);
}

static int parses(const char *body, int *t, int *r) {
    return fan_threshold_body_parse(body, strlen(body), 45, 85, t, r) == 0;
}

/* A bad request has no side effect on the fan: it used to pin 65 C, and persist it. */
static void test_threshold_body(void) {
    int t = -1, r = -1;
    CHECK(parses("65", &t, &r) && t == 65 && r == 0);
    CHECK(parses("45", &t, &r) && t == 45);
    CHECK(parses("85", &t, &r) && t == 85);
    CHECK(parses("0", &t, &r) && t == 0 && r == 0); /* the console's own back */
    CHECK(parses("70 30", &t, &r) && t == 70 && r == 30);
    CHECK(parses("70 0", &t, &r) && t == 70 && r == 0);
    CHECK(parses("  65\n", &t, &r) && t == 65); /* the lab CLI and curl add these */
    CHECK(parses("65 2\r\n", &t, &r) && t == 65 && r == 2);

    CHECK(!parses("", &t, &r));
    CHECK(!parses("abc", &t, &r));
    CHECK(!parses("-5", &t, &r));
    CHECK(!parses("44", &t, &r));
    CHECK(!parses("86", &t, &r));
    CHECK(!parses("255", &t, &r));
    CHECK(!parses("65abc", &t, &r));
    CHECK(!parses("65 abc", &t, &r));
    CHECK(!parses("65 -5", &t, &r));
    CHECK(!parses("65 30 7", &t, &r));
    CHECK(!parses("99999999999999999999", &t, &r));
    CHECK(!parses("65 99999999999999999999", &t, &r));
    CHECK(fan_threshold_body_parse(NULL, 0, 45, 85, &t, &r) == -1);
    CHECK(fan_threshold_body_parse("65\0x", 4, 45, 85, &t, &r) == -1);
    CHECK(fan_threshold_body_parse("65", 40, 45, 85, &t, &r) == -1); /* longer than any request */
}

static void test_range(void) {
    CHECK(fan_target_refusal(85, 91) == NULL);
    CHECK(fan_target_refusal(85, 0) == NULL); /* the console's own was never seen */
    CHECK(fan_target_refusal(70, 69) != NULL); /* the board whose own target is 69 */
    CHECK(fan_target_refusal(69, 69) == NULL);
    CHECK(strcmp(fan_target_refusal(85, 69), "fan_above_stock") == 0);
}

static void test_decoding(void) {
    uint8_t ext[FAN_EXT_LEN] = {0};
    uint32_t q8 = (uint32_t)(47.3 * 256);
    memcpy(ext + 0x10, &q8, 4);
    CHECK(fan_ext_filtered_tenths(ext) == 473);
    q8 = 5 * 256;
    memcpy(ext + 0x10, &q8, 4);
    CHECK(fan_ext_filtered_tenths(ext) == -1);
    q8 = 0xffffffffu;
    memcpy(ext + 0x10, &q8, 4);
    CHECK(fan_ext_filtered_tenths(ext) == -1);
    CHECK(fan_duty_q10_pct(0) == 0);
    CHECK(fan_duty_q10_pct(195) == 19); /* the idle duty the reference measured */
    CHECK(fan_duty_q10_pct(338) == 33);
    CHECK(fan_duty_q10_pct(1024) == 100);
    CHECK(fan_duty_q10_pct(5000) == 100);
}

int main(void) {
    test_set_keeps_the_profile();
    test_failed_get_writes_nothing();
    test_readback_is_checked();
    test_hex();
    test_watcher();
    test_reapply_conf();
    test_threshold_body();
    test_range();
    test_decoding();
    if (fails) {
        printf("fan_servo_selftest: %d failure(s)\n", fails);
        return 1;
    }
    printf("fan_servo_selftest: ok\n");
    return 0;
}
