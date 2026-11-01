/* The PC's keyboard and mouse as the PS5's own: virtual input devices driven over AVA1.
 *
 * Three management methods (protocol/ps5upload-ava1/MGMT_METHODS.md, app.toml 185-187):
 *
 *   vinput.caps     {} -> {"mouse":B,"keyboard":B,"open":B,"backend":"<name>"[,"reason":"<token>"]}
 *                   what this console can create (B = true/false), and whether a session is open.
 *   vinput.session  {"op":"open","id":N,"mouse":B,"keyboard":B} | {"op":"hb","id":N} | {"op":"close","id":N}
 *                   -> {"ok":B,"open":B,"mouse":B,"keyboard":B[,"err":"<token>"]}
 *                   open makes the devices asked for (keeping one already open, removing one no longer
 *                   asked for), hb keeps them, close lets everything go and removes them. `id` is the
 *                   owner's token: while a session is open only that owner's hb, report and close count
 *                   (another open takes the session over, letting the old owner's input go first).
 *   vinput.report   {"id":N,"r":"<hex>"} -> {"ok":B,"applied":N[,"err":"<token>"]}
 *                   `r` is 1..VINPUT_MAX_REPORTS reports of VINPUT_REPORT_LEN bytes each, applied in
 *                   order; a report is also a heartbeat.
 *
 * A report (16 bytes, little-endian; the page's binary kind 8 carries the same bytes):
 *   0     buttons  bit0 left, bit1 right, bit2 middle, bit3 back, bit4 forward
 *   1     mods     HID modifier byte: bit0 LCtrl 1 LShift 2 LAlt 3 LGUI 4 RCtrl 5 RShift 6 RAlt 7 RGUI
 *   2-3   dx       i16, relative
 *   4-5   dy       i16, relative (down is positive)
 *   6     wheel    i8, notches (up/away is positive)
 *   7     hwheel   i8, notches (right is positive)
 *   8-15  keys     HID keyboard usage IDs held (0 = none), at most 8
 * Buttons, mods and keys are the whole state (a report with none of them lets go of everything);
 * dx/dy/wheel are movement since the last report.
 *
 * Safety: a session with no heartbeat (hb or report) for VINPUT_HEARTBEAT_TIMEOUT_MS is let go and
 * its devices removed (vinput_tick, from a watchdog thread that runs only while a session is open);
 * so is one whose owner closes it, and every session when the helper stops (vinput_stop).
 *
 * The firmware side is a backend behind vinput_backend_t. The built-in one is a stub: it reports no
 * devices and refuses to open any ("vinput_unsupported"). The console's own devices
 * (src/vinput_ps5.c, libSceMouse / libSceKeyboard) replace it only when switched on (vinput_ps5.h:
 * off by default, FW 13.60 only). Everything here (the methods, ownership, heartbeat and release) is
 * real and host-tested (payload/tests/vinput_selftest.c). */
#ifndef PS5UPLOAD_VINPUT_H
#define PS5UPLOAD_VINPUT_H

#include <stddef.h>
#include <stdint.h>

#define VINPUT_REPORT_LEN 16
#define VINPUT_KEYS 8
#define VINPUT_MAX_REPORTS 64
#define VINPUT_HEARTBEAT_TIMEOUT_MS 2000LL
/* How often the watchdog looks. */
#define VINPUT_WATCHDOG_STEP_MS 250
/* A request body's bound: VINPUT_MAX_REPORTS reports in hex, and the keys around them. */
#define VINPUT_REQ_MAX (2u * VINPUT_REPORT_LEN * VINPUT_MAX_REPORTS + 256u)
/* Every answer fits this. */
#define VINPUT_OUT_MAX 256u

/* Mouse buttons (report byte 0). */
#define VINPUT_BTN_LEFT 0x01u
#define VINPUT_BTN_RIGHT 0x02u
#define VINPUT_BTN_MIDDLE 0x04u
#define VINPUT_BTN_BACK 0x08u
#define VINPUT_BTN_FORWARD 0x10u

/* Backend results. */
#define VINPUT_OK 0
#define VINPUT_E_UNSUPPORTED (-1) /* this firmware cannot (or we do not yet know how to) */
#define VINPUT_E_FAILED (-2)      /* it can, but this call failed */

typedef struct {
    uint8_t buttons;
    uint8_t mods;
    int16_t dx;
    int16_t dy;
    int8_t wheel;
    int8_t hwheel;
    uint8_t keys[VINPUT_KEYS];
} vinput_report_t;

/* The firmware's virtual devices. Every call is made with the module's lock held, one at a time. */
typedef struct {
    const char *name;
    /* What can be created: 1 or 0 each. */
    void (*caps)(int *mouse, int *keyboard);
    int (*mouse_open)(void);
    void (*mouse_close)(void);
    /* One mouse report: buttons held now, movement and wheel since the last. */
    int (*mouse_send)(uint8_t buttons, int dx, int dy, int wheel, int hwheel);
    int (*keyboard_open)(void);
    void (*keyboard_close)(void);
    /* The keyboard's whole state: modifiers and the keys held. */
    int (*keyboard_send)(uint8_t mods, const uint8_t keys[VINPUT_KEYS]);
} vinput_backend_t;

/* The backend in use: NULL restores the built-in one (the stub). Lets go of an open session first
 * and clears a vinput_stop. runtime.c sets it once, before first use (vinput_ps5.c); tests swap it. */
void vinput_set_backend(const vinput_backend_t *b);
const vinput_backend_t *vinput_backend(void);

/* Decodes one 16-byte report. */
void vinput_report_decode(const uint8_t raw[VINPUT_REPORT_LEN], vinput_report_t *out);
/* Decodes `hex` (2 * VINPUT_REPORT_LEN * n characters, either case) into at most `max` reports.
 * The count, or -1 when it is not whole reports of hex or holds more than max (or none). */
int vinput_reports_from_hex(const char *hex, size_t len, vinput_report_t *out, int max);

/* The three methods' bodies: each writes a JSON answer into out (cap >= VINPUT_OUT_MAX) and returns
 * its length, -1 only when cap cannot hold it. now_ms is CLOCK_MONOTONIC in ms. */
int vinput_caps_json(char *out, size_t cap);
int vinput_session_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vinput_report_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);

/* The heartbeat check: lets go of an open session whose last heartbeat is older than
 * VINPUT_HEARTBEAT_TIMEOUT_MS at now_ms. 1 when it let one go. */
int vinput_tick(long long now_ms);
/* 1 while a session is open. */
int vinput_is_open(void);
/* Lets go of every key and button and removes the devices (the helper is stopping). Also stops the
 * watchdog thread. Safe to call any time, more than once. */
void vinput_stop(void);
/* What the last release was for ("timeout", "close", "takeover", "stop", "backend"; "" before any). */
const char *vinput_last_release(void);

/* Tests: no watchdog thread (vinput_tick is called by hand). */
void vinput_test_no_watchdog(int on);
/* CLOCK_MONOTONIC in ms. */
long long vinput_now_ms(void);

#endif
