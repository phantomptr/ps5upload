/* Host-side tests for the virtual keyboard and mouse (vinput.c): the stub answers "not supported",
 * and over a fake backend the session opens, keeps, hands over and closes; reports decode and apply
 * in order; a missed heartbeat, a close, a takeover and the helper stopping each let go of every key
 * and button before the devices are removed; and the watchdog thread does it on its own.
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "vinput.h"
#include "vinput_ps5.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

#define B(s) s, strlen(s)
#define HAS(s) (strstr(g_out, s) != NULL)

static char g_out[VINPUT_OUT_MAX];

/* ---- a fake backend that records what it was asked ---- */

static int f_can_mouse = 1, f_can_keyboard = 1, f_fail_open;
static char f_log[4096];

static void logf_(const char *s) {
    size_t n = strlen(f_log);
    snprintf(f_log + n, sizeof f_log - n, "%s;", s);
}
static void f_caps(int *m, int *k) {
    *m = f_can_mouse;
    *k = f_can_keyboard;
}
static int f_mouse_open(void) {
    if (f_fail_open) return VINPUT_E_FAILED;
    logf_("mopen");
    return VINPUT_OK;
}
static void f_mouse_close(void) {
    logf_("mclose");
}
static int f_mouse_send(uint8_t b, int dx, int dy, int w, int hw) {
    char s[64];
    snprintf(s, sizeof s, "m%u,%d,%d,%d,%d", b, dx, dy, w, hw);
    logf_(s);
    return VINPUT_OK;
}
static int f_kbd_open(void) {
    if (f_fail_open) return VINPUT_E_FAILED;
    logf_("kopen");
    return VINPUT_OK;
}
static void f_kbd_close(void) {
    logf_("kclose");
}
static int f_kbd_send(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    char s[64];
    snprintf(s, sizeof s, "k%u,%u,%u", mods, keys[0], keys[1]);
    logf_(s);
    return VINPUT_OK;
}
static const vinput_backend_t FAKE = {
    "fake", f_caps, f_mouse_open, f_mouse_close, f_mouse_send, f_kbd_open, f_kbd_close, f_kbd_send,
};

static void reset(void) {
    vinput_set_backend(&FAKE);
    f_can_mouse = f_can_keyboard = 1;
    f_fail_open = 0;
    f_log[0] = 0;
}

static int session(const char *body, long long now) {
    return vinput_session_json(B(body), now, g_out, sizeof g_out);
}
static int report(const char *body, long long now) {
    return vinput_report_json(B(body), now, g_out, sizeof g_out);
}

/* 16 bytes as hex: buttons, mods, dx, dy, wheel, hwheel, keys[0], keys[1]. */
static void rep_hex(char *out, int buttons, int mods, int dx, int dy, int wheel, int hwheel, int k0, int k1) {
    uint8_t r[VINPUT_REPORT_LEN] = {0};
    r[0] = (uint8_t)buttons;
    r[1] = (uint8_t)mods;
    r[2] = (uint8_t)(dx & 0xff);
    r[3] = (uint8_t)((dx >> 8) & 0xff);
    r[4] = (uint8_t)(dy & 0xff);
    r[5] = (uint8_t)((dy >> 8) & 0xff);
    r[6] = (uint8_t)wheel;
    r[7] = (uint8_t)hwheel;
    r[8] = (uint8_t)k0;
    r[9] = (uint8_t)k1;
    for (int i = 0; i < VINPUT_REPORT_LEN; i++) snprintf(out + 2 * i, 3, "%02x", r[i]);
}

static void test_stub(void) {
    vinput_set_backend(NULL);
    CHECK(strcmp(vinput_backend()->name, "stub") == 0);
    CHECK(vinput_caps_json(g_out, sizeof g_out) > 0);
    CHECK(strcmp(g_out, "{\"mouse\":false,\"keyboard\":false,\"open\":false,\"backend\":\"stub\","
                        "\"reason\":\"vinput_unsupported\"}") == 0);
    CHECK(session("{\"op\":\"open\",\"id\":7,\"mouse\":true,\"keyboard\":true}", 0) > 0);
    CHECK(strcmp(g_out, "{\"ok\":false,\"open\":false,\"mouse\":false,\"keyboard\":false,"
                        "\"err\":\"vinput_unsupported\"}") == 0);
    CHECK(!vinput_is_open());
    CHECK(session("{\"op\":\"hb\",\"id\":7}", 0) > 0 && HAS("\"err\":\"vinput_not_open\""));
    char body[128], hex[40];
    rep_hex(hex, 1, 0, 3, 4, 0, 0, 4, 0);
    snprintf(body, sizeof body, "{\"id\":7,\"r\":\"%s\"}", hex);
    CHECK(report(body, 0) > 0 && strcmp(g_out, "{\"ok\":false,\"err\":\"vinput_not_open\"}") == 0);
    /* closing nothing is fine */
    CHECK(session("{\"op\":\"close\",\"id\":7}", 0) > 0 && HAS("\"ok\":true"));
}

static void test_decode(void) {
    char hex[40];
    vinput_report_t r[3];
    rep_hex(hex, VINPUT_BTN_LEFT | VINPUT_BTN_FORWARD, 0x22, -300, 32767, -1, 2, 0x04, 0xe1);
    CHECK(vinput_reports_from_hex(hex, 32, r, 3) == 1);
    CHECK(r[0].buttons == 0x11 && r[0].mods == 0x22 && r[0].dx == -300 && r[0].dy == 32767);
    CHECK(r[0].wheel == -1 && r[0].hwheel == 2 && r[0].keys[0] == 4 && r[0].keys[1] == 0xe1 && r[0].keys[7] == 0);
    /* upper case is hex too */
    for (char *p = hex; *p; p++)
        if (*p >= 'a' && *p <= 'f') *p = (char)(*p - 'a' + 'A');
    CHECK(vinput_reports_from_hex(hex, 32, r, 3) == 1 && r[0].dx == -300);
    /* not whole reports, not hex, none, or too many */
    CHECK(vinput_reports_from_hex(hex, 30, r, 3) == -1);
    CHECK(vinput_reports_from_hex("zz000000000000000000000000000000", 32, r, 3) == -1);
    CHECK(vinput_reports_from_hex("", 0, r, 3) == -1);
    char four[4 * 32 + 1] = {0};
    for (int i = 0; i < 4; i++) memcpy(four + 32 * i, hex, 32);
    CHECK(vinput_reports_from_hex(four, 128, r, 3) == -1);
}

static void test_open_report_close(void) {
    char body[512], a[40], b[40], c[40];
    reset();
    CHECK(vinput_caps_json(g_out, sizeof g_out) > 0 && HAS("\"mouse\":true,\"keyboard\":true,\"open\":false") &&
          !HAS("reason"));
    CHECK(session("{\"op\":\"open\",\"id\":5,\"mouse\":true,\"keyboard\":true}", 1000) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"open\":true,\"mouse\":true,\"keyboard\":true}") == 0);
    CHECK(strcmp(f_log, "mopen;kopen;") == 0);
    CHECK(vinput_is_open());
    /* three reports in order: move with left held + 'a', move again (keys unchanged: no keyboard
     * report), let go of everything */
    rep_hex(a, 1, 0, 3, -4, 0, 0, 4, 0);
    rep_hex(b, 1, 0, 1, 1, 1, 0, 4, 0);
    rep_hex(c, 0, 0, 0, 0, 0, 0, 0, 0);
    snprintf(body, sizeof body, "{\"id\":5,\"r\":\"%s%s%s\"}", a, b, c);
    f_log[0] = 0;
    CHECK(report(body, 1100) > 0 && strcmp(g_out, "{\"ok\":true,\"applied\":3}") == 0);
    CHECK(strcmp(f_log, "m1,3,-4,0,0;k0,4,0;m1,1,1,1,0;m0,0,0,0,0;k0,0,0;") == 0);
    /* another owner's report and heartbeat do not count */
    CHECK(report("{\"id\":6,\"r\":\"00000000000000000000000000000000\"}", 1200) > 0 &&
          HAS("\"err\":\"vinput_not_owner\""));
    CHECK(session("{\"op\":\"hb\",\"id\":6}", 1200) > 0 && HAS("\"err\":\"vinput_not_owner\""));
    CHECK(session("{\"op\":\"close\",\"id\":6}", 1200) > 0 && HAS("\"err\":\"vinput_not_owner\"") && vinput_is_open());
    /* bad bodies */
    CHECK(report("{\"id\":5,\"r\":\"0102\"}", 1200) > 0 && HAS("bad_request"));
    CHECK(report("{\"r\":\"00000000000000000000000000000000\"}", 1200) > 0 && HAS("bad_request"));
    CHECK(session("{\"op\":\"jump\",\"id\":5}", 1200) > 0 && HAS("bad_request"));
    CHECK(session("{\"op\":\"open\",\"id\":0,\"mouse\":true}", 1200) > 0 && HAS("bad_request"));
    CHECK(session("{\"op\":\"open\",\"id\":5}", 1200) > 0 && HAS("bad_request") && vinput_is_open());
    CHECK(session("{\"op\":\"hb\",\"id\":5,\"mouse\":\"yes\"}", 1200) > 0 && HAS("bad_request"));
    /* hold a key, then close: it is let go before the device goes */
    rep_hex(a, 2, 0x02, 0, 0, 0, 0, 0x1a, 0);
    snprintf(body, sizeof body, "{\"id\":5,\"r\":\"%s\"}", a);
    CHECK(report(body, 1300) > 0 && HAS("\"ok\":true"));
    f_log[0] = 0;
    CHECK(session("{\"op\":\"close\",\"id\":5}", 1400) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"open\":false,\"mouse\":false,\"keyboard\":false}") == 0);
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;k0,0,0;kclose;") == 0);
    CHECK(strcmp(vinput_last_release(), "close") == 0 && !vinput_is_open());
}

static void test_heartbeat(void) {
    char body[128], a[40];
    reset();
    CHECK(session("{\"op\":\"open\",\"id\":9,\"mouse\":true,\"keyboard\":true}", 0) > 0 && HAS("\"ok\":true"));
    rep_hex(a, 1, 0, 0, 0, 0, 0, 0x2c, 0);
    snprintf(body, sizeof body, "{\"id\":9,\"r\":\"%s\"}", a);
    CHECK(report(body, 100) > 0);
    /* heartbeats keep it */
    CHECK(session("{\"op\":\"hb\",\"id\":9}", 1500) > 0 && HAS("\"ok\":true,\"open\":true"));
    CHECK(vinput_tick(1500 + VINPUT_HEARTBEAT_TIMEOUT_MS) == 0 && vinput_is_open());
    /* a report is a heartbeat too */
    CHECK(report(body, 3000) > 0 && HAS("\"ok\":true"));
    CHECK(vinput_tick(3000 + VINPUT_HEARTBEAT_TIMEOUT_MS) == 0);
    /* then silence: let go of the held button and space, and the devices go */
    f_log[0] = 0;
    CHECK(vinput_tick(3001 + VINPUT_HEARTBEAT_TIMEOUT_MS) == 1);
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;k0,0,0;kclose;") == 0);
    CHECK(strcmp(vinput_last_release(), "timeout") == 0 && !vinput_is_open());
    CHECK(vinput_tick(99999) == 0);
    /* a request after the timeout (no watchdog ran) finds it gone, not still open */
    CHECK(session("{\"op\":\"open\",\"id\":9,\"mouse\":true}", 10000) > 0 && HAS("\"ok\":true"));
    f_log[0] = 0;
    CHECK(report(body, 10000 + VINPUT_HEARTBEAT_TIMEOUT_MS + 1) > 0 && HAS("vinput_not_open"));
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;") == 0);
}

static void test_takeover_and_devices(void) {
    char body[128], a[40];
    reset();
    CHECK(session("{\"op\":\"open\",\"id\":1,\"mouse\":true,\"keyboard\":true}", 0) > 0);
    rep_hex(a, 4, 0, 0, 0, 0, 0, 0x29, 0);
    snprintf(body, sizeof body, "{\"id\":1,\"r\":\"%s\"}", a);
    CHECK(report(body, 10) > 0);
    /* a second owner: the first one's middle button and Esc are let go, then it gets the mouse only */
    f_log[0] = 0;
    CHECK(session("{\"op\":\"open\",\"id\":2,\"mouse\":true,\"keyboard\":false}", 20) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"open\":true,\"mouse\":true,\"keyboard\":false}") == 0);
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;k0,0,0;kclose;mopen;") == 0);
    /* the same owner adds the keyboard, then drops the mouse (mixed -> keyboard only) */
    f_log[0] = 0;
    CHECK(session("{\"op\":\"open\",\"id\":2,\"mouse\":true,\"keyboard\":true}", 30) > 0 && HAS("\"keyboard\":true"));
    CHECK(strcmp(f_log, "kopen;") == 0);
    f_log[0] = 0;
    CHECK(session("{\"op\":\"open\",\"id\":2,\"mouse\":false,\"keyboard\":true}", 40) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"open\":true,\"mouse\":false,\"keyboard\":true}") == 0);
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;") == 0);
    /* only the keyboard is open: a report's mouse part is not sent anywhere */
    rep_hex(a, 1, 0, 5, 5, 0, 0, 0x04, 0);
    snprintf(body, sizeof body, "{\"id\":2,\"r\":\"%s\"}", a);
    f_log[0] = 0;
    CHECK(report(body, 50) > 0 && strcmp(f_log, "k0,4,0;") == 0);
    /* a console that has only a mouse: asking for both opens the mouse and says the keyboard is not there */
    reset();
    f_can_keyboard = 0;
    CHECK(session("{\"op\":\"open\",\"id\":3,\"mouse\":true,\"keyboard\":true}", 0) > 0);
    CHECK(strcmp(g_out, "{\"ok\":false,\"open\":true,\"mouse\":true,\"keyboard\":false,"
                        "\"err\":\"vinput_unsupported\"}") == 0);
    /* a create that fails */
    reset();
    f_fail_open = 1;
    CHECK(session("{\"op\":\"open\",\"id\":3,\"mouse\":true}", 0) > 0 && HAS("\"err\":\"vinput_failed\"") &&
          HAS("\"open\":false"));
}

static void test_stop(void) {
    reset();
    CHECK(session("{\"op\":\"open\",\"id\":4,\"mouse\":true,\"keyboard\":true}", 0) > 0 && vinput_is_open());
    f_log[0] = 0;
    vinput_stop();
    CHECK(strcmp(f_log, "m0,0,0,0,0;mclose;k0,0,0;kclose;") == 0 && !vinput_is_open());
    CHECK(strcmp(vinput_last_release(), "stop") == 0);
    vinput_stop(); /* twice is fine */
    CHECK(session("{\"op\":\"open\",\"id\":4,\"mouse\":true}", 0) > 0 && HAS("\"err\":\"vinput_stopping\""));
}

/* The real watchdog thread lets go of a session with no heartbeat on its own. */
static void test_watchdog_thread(void) {
    reset();
    vinput_test_no_watchdog(0);
    CHECK(session("{\"op\":\"open\",\"id\":11,\"mouse\":true}", vinput_now_ms()) > 0 && vinput_is_open());
    long long until = vinput_now_ms() + VINPUT_HEARTBEAT_TIMEOUT_MS + 2000;
    while (vinput_is_open() && vinput_now_ms() < until) usleep(50 * 1000);
    CHECK(!vinput_is_open());
    CHECK(strcmp(vinput_last_release(), "timeout") == 0);
    CHECK(strstr(f_log, "m0,0,0,0,0;mclose;") != NULL);
    vinput_test_no_watchdog(1);
}

static void test_bounds(void) {
    /* the largest request fits and every answer fits VINPUT_OUT_MAX */
    static char body[VINPUT_REQ_MAX + 64];
    int at;
    reset();
    CHECK(session("{\"op\":\"open\",\"id\":18446744073709551615,\"mouse\":true,\"keyboard\":true}", 0) > 0 &&
          HAS("\"ok\":true"));
    at = snprintf(body, sizeof body, "{\"id\":18446744073709551615,\"r\":\"");
    for (int i = 0; i < VINPUT_MAX_REPORTS; i++) {
        rep_hex(body + at, 0, 0, 1, 0, 0, 0, 0, 0);
        at += 32;
    }
    snprintf(body + at, sizeof body - (size_t)at, "\"}");
    CHECK(strlen(body) <= VINPUT_REQ_MAX);
    CHECK(report(body, 1) > 0 && strcmp(g_out, "{\"ok\":true,\"applied\":64}") == 0);
    /* one report more is refused */
    rep_hex(body + at, 0, 0, 1, 0, 0, 0, 0, 0);
    snprintf(body + at + 32, sizeof body - (size_t)at - 32, "\"}");
    CHECK(report(body, 1) > 0 && HAS("bad_request"));
    /* a small buffer is -1, never a cut answer */
    char tiny[8];
    CHECK(vinput_caps_json(tiny, sizeof tiny) == -1);
    vinput_stop();
}

/* The console's own devices are allowed only on the firmware they were probed on. */
static void test_firmware_allow_list(void) {
    CHECK(vinput_ps5_firmware_allowed("13.60"));
    CHECK(!vinput_ps5_firmware_allowed("13.40"));
    CHECK(!vinput_ps5_firmware_allowed("9.60"));
    CHECK(!vinput_ps5_firmware_allowed("unknown"));
    CHECK(!vinput_ps5_firmware_allowed(""));
    CHECK(!vinput_ps5_firmware_allowed(NULL));
}

int main(void) {
    test_firmware_allow_list();
    /* fake clocks everywhere but test_watchdog_thread: no thread may compare them with the real one */
    vinput_test_no_watchdog(1);
    test_stub();
    test_decode();
    test_open_report_close();
    test_heartbeat();
    test_takeover_and_devices();
    test_stop();
    test_watchdog_thread();
    test_bounds();
    if (failures) {
        fprintf(stderr, "vinput selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("vinput selftest: ok\n");
    return 0;
}
