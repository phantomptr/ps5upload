/* Play Together (#641): virtual DualSense controllers for Players 2-4, driven over AVA1.
 *
 * Each slot (2, 3, 4: the player the engine hands a guest) is one virtual controller the helper
 * makes with ioctls on /dev/hid, owned by a guest's token (128 bits, 32 hex characters, from the
 * engine). The pad is made for a signed-in user (the foreground user, else the first one in
 * /user/home): the console then asks "Who's using this controller?" on the first PS press and the
 * guest picks Quick Play (a one-time guest) or a user. A pad lives as long as the helper.
 *
 * Seven management methods (protocol/ps5upload-ava1/MGMT_METHODS.md, app.toml 188-194). A refusal is
 * {"ok":false,"err":"<token>"[,"code":-errno]} (kept as the call's error cause, like vinput.*):
 *
 *   vpad.open   {"slot":2..4,"token":"<32 hex>"[,"user":N]}
 *               -> {"ok":true,"slot":S,"handle":H,"user":U,"resumed":B}
 *               makes the pad (user N, or the foreground user when absent or 0). The same token on a
 *               live slot is a resume: nothing is made again, the pad keeps its place on the console.
 *               err vpad_busy (another token holds the slot), vpad_no_user (nobody to make it for),
 *               vpad_unsupported (no /dev/hid), vpad_failed (+code), vpad_stopping, bad_request.
 *   vpad.state  {"slot":S,"token":T,"seq":N[,"buttons":B,"lx":..,"ly":..,"rx":..,"ry":..,"l2":..,"r2":..]
 *                [,"touch":[{"id":I,"x":X,"y":Y}, at most 2]][,"gyro":[x,y,z]][,"accel":[x,y,z]]}
 *               -> {"ok":true,"applied":B}
 *               the whole controller state, inserted at once. Absent fields are at rest (sticks 128,
 *               triggers 0, no touch, gyro 0, accel {0,1,0}). `seq` grows per slot: a seq not above
 *               the last applied one is ignored ("applied":false). A state is also a heartbeat.
 *               err vpad_not_open, vpad_not_owner, vpad_failed (+code), bad_request.
 *   vpad.states {"items":[<vpad.state bodies>, at most VPAD_MAX_BATCH]}
 *               -> {"ok":true,"applied":N,"stale":M,"failed":[{"slot":S,"err":"<token>"}..]}
 *   vpad.hb     {"slot":S,"token":T[,"live":true]} -> {"ok":true,"neutral":B}
 *               keeps the pad (the 90 s timer). "live":true also says the guest is still there with
 *               its last state (the 400 ms timer), for an engine that sends no unchanged states.
 *   vpad.close  {"slot":S,"token":T} -> {"ok":true}   puts the pad at rest and removes it.
 *               Closing a free slot is ok; err vpad_not_owner.
 *   vpad.output {"slot":S} -> {"ok":true,"slot":S,"motors":[l,r],"color":[r,g,b]}
 *               the rumble and light bar the console last asked of that pad. err vpad_not_open,
 *               vpad_failed (+code).
 *   vpad.status {} -> {"ok":true,"slots":[{"slot":S,"player":S,"open":B[,"owner":"<hash>","user":U,
 *                "since_ms":N,"last_state_ms":N|-1,"neutral":B,"neutral_ms":N]}..]}
 *               ages in ms; owner is a hash of the token (never the token).
 *
 * Watchdog (vpad_tick, from a thread that runs while any pad is open): a slot with no state (and no
 * "live" hb) for VPAD_NEUTRAL_MS gets one neutral state (no stuck stick or button while a guest's
 * network drops); a slot with no state or hb for VPAD_IDLE_DELETE_MS loses its pad. vpad_stop (the
 * helper stopping, a takeover) removes every pad.
 *
 * The ioctls are those of ProsperoSunshine's src/platform/ps5/pad.c (see src/vpad.c). The device
 * layer is behind vpad_hid_t so a host test can fake /dev/hid (payload/tests/vpad_selftest.c). */
#ifndef PS5UPLOAD_VPAD_H
#define PS5UPLOAD_VPAD_H

#include <stddef.h>
#include <stdint.h>

#define VPAD_FIRST_SLOT 2
#define VPAD_LAST_SLOT 4
#define VPAD_SLOTS (VPAD_LAST_SLOT - VPAD_FIRST_SLOT + 1)
#define VPAD_TOKEN_HEX 32
#define VPAD_MAX_TOUCH 2
#define VPAD_MAX_BATCH 16
#define VPAD_NEUTRAL_MS 400LL
#define VPAD_IDLE_DELETE_MS 90000LL
/* How often the watchdog looks: a silent pad is at rest 400-450 ms after its last state. */
#define VPAD_WATCHDOG_STEP_MS 50
/* A request body's bound (a full batch with touch and motion fits), and every answer's. */
#define VPAD_REQ_MAX 8192u
#define VPAD_OUT_MAX 1024u

/* The device's state and description blocks, and its ioctls on /dev/hid. */
#define VPAD_BLOCK 0xA8
#define VPAD_IOCTL_ADD 0xC018482AUL
#define VPAD_IOCTL_INSERT 0x8018482CUL
#define VPAD_IOCTL_DELETE 0x80104850UL
#define VPAD_IOCTL_OUTPUT 0x80684830UL

/* Buttons (libScePad's bits; the state's u32 at 0x0C). */
#define VPAD_BTN_CREATE 0x00000001u
#define VPAD_BTN_L3 0x00000002u
#define VPAD_BTN_R3 0x00000004u
#define VPAD_BTN_OPTIONS 0x00000008u
#define VPAD_BTN_UP 0x00000010u
#define VPAD_BTN_RIGHT 0x00000020u
#define VPAD_BTN_DOWN 0x00000040u
#define VPAD_BTN_LEFT 0x00000080u
#define VPAD_BTN_L2 0x00000100u
#define VPAD_BTN_R2 0x00000200u
#define VPAD_BTN_L1 0x00000400u
#define VPAD_BTN_R1 0x00000800u
#define VPAD_BTN_TRIANGLE 0x00001000u
#define VPAD_BTN_CIRCLE 0x00002000u
#define VPAD_BTN_CROSS 0x00004000u
#define VPAD_BTN_SQUARE 0x00008000u
#define VPAD_BTN_PS 0x00010000u
#define VPAD_BTN_TOUCHPAD 0x00100000u

typedef struct {
    uint8_t id;
    uint16_t x, y; /* the touchpad is 1920 x 1080 */
} vpad_touch_t;

typedef struct {
    uint32_t buttons;
    uint8_t lx, ly, rx, ry; /* 0..255, 128 = centre */
    uint8_t l2, r2;         /* 0..255 */
    uint8_t touch_n;        /* 0..VPAD_MAX_TOUCH */
    vpad_touch_t touch[VPAD_MAX_TOUCH];
    float gyro[3];  /* angular velocity */
    float accel[3]; /* acceleration; {0,1,0} at rest */
} vpad_state_t;

/* The device layer: /dev/hid, or a test's fake. Each returns 0 / an fd, or -errno. */
typedef struct {
    int (*open_hid)(void);
    int (*ioctl_hid)(int fd, unsigned long req, void *arg);
    void (*close_hid)(int fd);
} vpad_hid_t;

/* The device layer in use: NULL restores /dev/hid. Removes every pad first and clears a vpad_stop. */
void vpad_set_hid(const vpad_hid_t *h);

/* Who a new pad is for: a signed-in user id, 0 when nobody. */
typedef uint32_t (*vpad_user_fn)(void);
/* The first user folder (8 hex digits, not 0) in `home_dir` (/user/home), 0 when none. */
uint32_t vpad_home_user(const char *home_dir);

/* A state at rest, and the 0xA8-byte block it is inserted as. */
void vpad_state_neutral(vpad_state_t *s);
void vpad_state_encode(const vpad_state_t *s, uint8_t block[VPAD_BLOCK]);

/* The methods' bodies: each writes a JSON answer into out (cap >= VPAD_OUT_MAX) and returns its
 * length, -1 only when cap cannot hold it. now_ms is CLOCK_MONOTONIC in ms. vpad.open asks
 * pick_user (outside the module's lock: it may wait for the Sony API lock) only when it makes a pad
 * and the body names no user; NULL means /user/home. */
int vpad_open_json(const char *body, size_t n, long long now_ms, vpad_user_fn pick_user, char *out, size_t cap);
int vpad_state_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vpad_states_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vpad_hb_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vpad_close_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vpad_output_json(const char *body, size_t n, long long now_ms, char *out, size_t cap);
int vpad_status_json(long long now_ms, char *out, size_t cap);

/* The watchdog's step at now_ms: rests silent pads, removes abandoned ones. Returns how many pads it
 * rested plus 100 x how many it removed (tests). */
int vpad_tick(long long now_ms);
/* How many pads are open. */
int vpad_open_count(void);
/* Removes every pad (the helper is stopping). Later opens answer vpad_stopping. Safe any time. */
void vpad_stop(void);

/* Tests: no watchdog thread (vpad_tick is called by hand). */
void vpad_test_no_watchdog(int on);
/* CLOCK_MONOTONIC in ms. */
long long vpad_now_ms(void);

#endif
