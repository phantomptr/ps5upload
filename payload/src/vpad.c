/* Play Together's virtual controllers (Players 2-4): see include/vpad.h.
 *
 * The /dev/hid requests (ADD, INSERT, DELETE and OUTPUT, the description and state block layouts,
 * the output fields) are from ProsperoSunshine (https://github.com/Darkatek7/ProsperoSunshine,
 * Darkatek7), src/platform/ps5/pad.c, GPL-3.0-only, itself derived from ps5-mcp's RemoteEye. They
 * are written anew here, around slots, owners and a watchdog of our own. This file is GPL-3.0-only
 * (ps5upload is GPL-3.0-or-later, which takes it in as GPL-3.0); see LICENSES/README.md. */
#include "vpad.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#include "proc_identity.h"
#include "webui_codec.h"

/* ── /dev/hid ─────────────────────────────────────────────────────────────────────────────────── */

static int hid_open(void) {
    int fd = open("/dev/hid", O_RDONLY);
    return fd >= 0 ? fd : (errno ? -errno : -EIO);
}
static int hid_ioctl(int fd, unsigned long req, void *arg) {
    return ioctl(fd, req, arg) == 0 ? 0 : (errno ? -errno : -EIO);
}
static void hid_close(int fd) {
    close(fd);
}
static const vpad_hid_t HID = {hid_open, hid_ioctl, hid_close};

/* The requests; each one's size is the one its ioctl number carries (bits 16-28). */
typedef struct {
    uint32_t op; /* 3: add a device */
    uint32_t pad;
    uint64_t desc;       /* -> VPAD_BLOCK bytes: class @0, user id @0x1C, subtype @0x20 */
    uint64_t handle_out; /* -> int */
} add_req_t;
typedef struct {
    uint32_t handle;
    uint32_t pad;
    uint64_t state; /* -> VPAD_BLOCK bytes, the whole state */
    uint8_t zero;
    uint8_t pad2[7];
} insert_req_t;
typedef struct {
    uint8_t op; /* 5: remove a device */
    uint8_t pad[7];
    uint32_t handle;
    uint32_t pad2;
} delete_req_t;
typedef struct {
    uint32_t handle;
    uint32_t pad;
    /* -> flag_a (u32), flags (u8[2]), motors (u8[2]), color (u8[3]), triggers (u8[24]), six u32
     * values, flag_b (u32): the kernel writes each through its own pointer. */
    uint64_t fields[12];
} output_req_t;
_Static_assert(sizeof(add_req_t) == ((VPAD_IOCTL_ADD >> 16) & 0x1FFF), "ADD request size");
_Static_assert(sizeof(insert_req_t) == ((VPAD_IOCTL_INSERT >> 16) & 0x1FFF), "INSERT request size");
_Static_assert(sizeof(delete_req_t) == ((VPAD_IOCTL_DELETE >> 16) & 0x1FFF), "DELETE request size");
_Static_assert(sizeof(output_req_t) == ((VPAD_IOCTL_OUTPUT >> 16) & 0x1FFF), "OUTPUT request size");

#define DESC_CLASS 0x00
#define DESC_USER 0x1C
#define DESC_SUBTYPE 0x20
#define ST_BUTTONS 0x0C
#define ST_STICKS 0x10
#define ST_TRIGGERS 0x14
#define ST_PRESENT 0x17
#define ST_TOUCH_N 0x1C
#define ST_TOUCH 0x20
#define ST_TOUCH_SIZE 8
#define ST_ORIENTATION 0x78
#define ST_GYRO 0x88
#define ST_ACCEL 0x94

/* ── State blocks ─────────────────────────────────────────────────────────────────────────────── */

void vpad_state_neutral(vpad_state_t *s) {
    memset(s, 0, sizeof *s);
    s->lx = s->ly = s->rx = s->ry = 128;
    s->accel[1] = 1.0f;
}

static void put_u16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

void vpad_state_encode(const vpad_state_t *s, uint8_t block[VPAD_BLOCK]) {
    static const float orientation[4] = {0.0f, 0.0f, 0.0f, 1.0f};
    uint8_t n = s->touch_n > VPAD_MAX_TOUCH ? VPAD_MAX_TOUCH : s->touch_n;
    memset(block, 0, VPAD_BLOCK);
    block[ST_BUTTONS] = (uint8_t)s->buttons;
    block[ST_BUTTONS + 1] = (uint8_t)(s->buttons >> 8);
    block[ST_BUTTONS + 2] = (uint8_t)(s->buttons >> 16);
    block[ST_BUTTONS + 3] = (uint8_t)(s->buttons >> 24);
    block[ST_STICKS] = s->lx;
    block[ST_STICKS + 1] = s->ly;
    block[ST_STICKS + 2] = s->rx;
    block[ST_STICKS + 3] = s->ry;
    block[ST_TRIGGERS] = s->l2;
    block[ST_TRIGGERS + 1] = s->r2;
    block[ST_PRESENT] = 1;
    block[ST_TOUCH_N] = n;
    for (uint8_t i = 0; i < n; i++) {
        uint8_t *t = block + ST_TOUCH + i * ST_TOUCH_SIZE;
        t[0] = s->touch[i].id;
        put_u16(t + 4, s->touch[i].x);
        put_u16(t + 6, s->touch[i].y);
    }
    /* The console is little-endian like every host the tests run on: floats go in as they are. */
    memcpy(block + ST_ORIENTATION, orientation, sizeof orientation);
    memcpy(block + ST_GYRO, s->gyro, sizeof s->gyro);
    memcpy(block + ST_ACCEL, s->accel, sizeof s->accel);
}

/* ── Slots ────────────────────────────────────────────────────────────────────────────────────── */

typedef struct {
    int open;
    int fd;
    int handle;
    uint32_t user;
    char token[VPAD_TOKEN_HEX + 1];
    int have_seq;
    uint64_t seq;           /* the last applied state's */
    vpad_state_t last;      /* the owner's last state */
    long long opened_ms;
    long long last_state_ms; /* -1: none yet */
    long long last_live_ms;  /* the last state or "live" hb: the neutral timer */
    long long last_alive_ms; /* the last open, state or hb: the removal timer */
    long long neutral_ms;
    int neutral; /* at rest since neutral_ms (made so, or rested by the watchdog) */
} slot_t;

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static slot_t g_slots[VPAD_SLOTS];
static const vpad_hid_t *g_hid = &HID;
static int g_stopped;
static int g_wd_running;
static int g_no_watchdog;

long long vpad_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int player_of(const slot_t *s) {
    return (int)(s - g_slots) + VPAD_FIRST_SLOT;
}

static int insert_locked(slot_t *s, const vpad_state_t *st) {
    uint8_t block[VPAD_BLOCK];
    insert_req_t r;
    vpad_state_encode(st, block);
    memset(&r, 0, sizeof r);
    r.handle = (uint32_t)s->handle;
    r.state = (uint64_t)(uintptr_t)block;
    return g_hid->ioctl_hid(s->fd, VPAD_IOCTL_INSERT, &r);
}

static int rest_locked(slot_t *s) {
    vpad_state_t st;
    vpad_state_neutral(&st);
    return insert_locked(s, &st);
}

/* Puts the pad at rest, removes it and frees the slot. Lock held. */
static void remove_locked(slot_t *s, const char *why) {
    delete_req_t d;
    int rc;
    if (!s->open) return;
    (void)rest_locked(s); /* nothing held when it goes */
    memset(&d, 0, sizeof d);
    d.op = 5;
    d.handle = (uint32_t)s->handle;
    rc = g_hid->ioctl_hid(s->fd, VPAD_IOCTL_DELETE, &d);
    g_hid->close_hid(s->fd);
    fprintf(stderr, "[vpad] player %d: pad %d removed (%s)%s\n", player_of(s), s->handle, why,
            rc ? " (the console refused the removal)" : "");
    memset(s, 0, sizeof *s);
    s->fd = -1;
}

static void remove_all_locked(const char *why) {
    for (int i = 0; i < VPAD_SLOTS; i++) remove_locked(&g_slots[i], why);
}

static int open_count_locked(void) {
    int n = 0;
    for (int i = 0; i < VPAD_SLOTS; i++) n += g_slots[i].open;
    return n;
}

static int tick_locked(long long now_ms) {
    int rested = 0, removed = 0;
    for (int i = 0; i < VPAD_SLOTS; i++) {
        slot_t *s = &g_slots[i];
        if (!s->open) continue;
        if (now_ms - s->last_alive_ms > VPAD_IDLE_DELETE_MS) {
            remove_locked(s, "no heartbeat for 90 s");
            removed++;
            continue;
        }
        if (!s->neutral && now_ms - s->last_live_ms > VPAD_NEUTRAL_MS) {
            int rc = rest_locked(s);
            s->neutral = 1;
            s->neutral_ms = now_ms;
            rested++;
            fprintf(stderr, "[vpad] player %d: no input for %lld ms: at rest%s\n", player_of(s),
                    now_ms - s->last_live_ms, rc ? " (insert failed)" : "");
        }
    }
    return rested + 100 * removed;
}

static void *watchdog_main(void *arg) {
    (void)arg;
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    for (;;) {
        usleep(VPAD_WATCHDOG_STEP_MS * 1000u);
        pthread_mutex_lock(&g_lock);
        (void)tick_locked(vpad_now_ms());
        if (!open_count_locked()) {
            g_wd_running = 0;
            pthread_mutex_unlock(&g_lock);
            return NULL;
        }
        pthread_mutex_unlock(&g_lock);
    }
}

/* Runs the watchdog while a pad is open (it ends itself once none is). Lock held. */
static void watchdog_ensure_locked(void) {
    pthread_t t;
    pthread_attr_t attr;
    if (g_wd_running || g_no_watchdog) return;
    if (pthread_attr_init(&attr) != 0) return;
    (void)pthread_attr_setstacksize(&attr, 64u * 1024u);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&t, &attr, watchdog_main, NULL) == 0) g_wd_running = 1;
    pthread_attr_destroy(&attr);
    /* No thread: pads still go on close and when the helper stops; vpad.status shows the ages. */
}

void vpad_set_hid(const vpad_hid_t *h) {
    pthread_mutex_lock(&g_lock);
    remove_all_locked("device layer replaced");
    g_hid = h ? h : &HID;
    g_stopped = 0;
    pthread_mutex_unlock(&g_lock);
}

void vpad_test_no_watchdog(int on) {
    g_no_watchdog = on;
}

int vpad_tick(long long now_ms) {
    int r;
    pthread_mutex_lock(&g_lock);
    r = tick_locked(now_ms);
    pthread_mutex_unlock(&g_lock);
    return r;
}

int vpad_open_count(void) {
    int n;
    pthread_mutex_lock(&g_lock);
    n = open_count_locked();
    pthread_mutex_unlock(&g_lock);
    return n;
}

void vpad_stop(void) {
    pthread_mutex_lock(&g_lock);
    g_stopped = 1;
    remove_all_locked("helper stopping");
    /* The watchdog sees no pad at its next step and ends. */
    pthread_mutex_unlock(&g_lock);
}

uint32_t vpad_home_user(const char *home_dir) {
    DIR *d = opendir(home_dir);
    struct dirent *de;
    uint32_t best = 0;
    if (!d) return 0;
    while ((de = readdir(d)) != NULL) {
        const char *n = de->d_name;
        uint32_t v = 0;
        int ok = strlen(n) == 8;
        for (int i = 0; ok && i < 8; i++) {
            char c = n[i];
            int h = c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
            if (h < 0) ok = 0;
            v = v << 4 | (uint32_t)(h < 0 ? 0 : h);
        }
        if (ok && v && (!best || v < best)) best = v;
    }
    closedir(d);
    return best;
}

/* ── Request parsing ──────────────────────────────────────────────────────────────────────────── */

static int is_ws(char c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}
static const char *skip_ws(const char *p, const char *e) {
    while (p < e && is_ws(*p)) p++;
    return p;
}

/* Past one JSON value at p (object, array, string or scalar), or NULL. */
static const char *skip_value(const char *p, const char *e) {
    if (p >= e) return NULL;
    if (*p == '"') {
        for (p++; p < e; p++) {
            if (*p == '\\') {
                if (++p >= e) return NULL;
            } else if (*p == '"') {
                return p + 1;
            }
        }
        return NULL;
    }
    if (*p == '{' || *p == '[') {
        int depth = 0;
        while (p < e) {
            if (*p == '"') {
                p = skip_value(p, e);
                if (!p) return NULL;
                continue;
            }
            if (*p == '{' || *p == '[') {
                depth++;
            } else if (*p == '}' || *p == ']') {
                if (--depth == 0) return p + 1;
            }
            p++;
        }
        return NULL;
    }
    while (p < e && *p != ',' && *p != ']' && *p != '}' && !is_ws(*p)) p++;
    return p;
}

/* The next element of the JSON array `arr` (its whole text, '[' to ']'): 1 with *el and *el_len, 0 at
 * its end, -1 when malformed. *pos starts at 0. */
static int array_next(const char *arr, size_t len, size_t *pos, const char **el, size_t *el_len) {
    const char *e = arr + len, *p, *q;
    if (*pos >= len) return 0;
    if (*pos == 0) {
        p = skip_ws(arr, e);
        if (p >= e || *p != '[') return -1;
        p = skip_ws(p + 1, e);
    } else {
        p = skip_ws(arr + *pos, e);
        if (p < e && *p == ']') {
            *pos = len;
            return 0;
        }
        if (p >= e || *p != ',') return -1;
        p = skip_ws(p + 1, e);
    }
    if (p < e && *p == ']') {
        *pos = len;
        return 0;
    }
    q = skip_value(p, e);
    if (!q || q == p) return -1;
    *el = p;
    *el_len = (size_t)(q - p);
    *pos = (size_t)(q - arr);
    return 1;
}

/* An optional unsigned field no larger than max: 0 ok (absent keeps *out), -1 bad. */
static int opt_uint(const char *b, size_t n, const char *key, uint64_t max, uint64_t *out) {
    uint64_t v = 0;
    int r = webui_json_u64(b, n, key, &v);
    if (r == 0) return 0;
    if (r < 0 || v > max) return -1;
    *out = v;
    return 0;
}

static int opt_u8(const char *b, size_t n, const char *key, uint8_t *out) {
    uint64_t v = *out;
    if (opt_uint(b, n, key, 255, &v) < 0) return -1;
    *out = (uint8_t)v;
    return 0;
}

/* An optional array of three numbers: 0 ok (absent or null keeps out), -1 bad. */
static int opt_vec3(const char *b, size_t n, const char *key, float out[3]) {
    const char *arr, *p, *e;
    size_t len;
    float v[3];
    int r = webui_json_array(b, n, key, &arr, &len);
    if (r == 0) return 0;
    if (r < 0) return webui_json_null(b, n, key) ? 0 : -1;
    p = arr + 1;
    e = arr + len - 1; /* the closing ']' */
    for (int i = 0; i < 3; i++) {
        char num[40];
        size_t k = 0;
        char *end;
        double d;
        p = skip_ws(p, e);
        while (p < e && k + 1 < sizeof num && ((*p >= '0' && *p <= '9') || *p == '-' || *p == '+' || *p == '.' || *p == 'e' || *p == 'E'))
            num[k++] = *p++;
        num[k] = '\0';
        if (!k || (p < e && !is_ws(*p) && *p != ',')) return -1;
        d = strtod(num, &end);
        if (*end || d != d || d > 1e6 || d < -1e6) return -1;
        v[i] = (float)d;
        p = skip_ws(p, e);
        if (i < 2) {
            if (p >= e || *p != ',') return -1;
            p++;
        }
    }
    if (skip_ws(p, e) != e) return -1;
    memcpy(out, v, sizeof v);
    return 0;
}

/* The slot (2..4): 0, or -1. */
static int slot_of(const char *b, size_t n, int *slot) {
    uint64_t v = 0;
    if (webui_json_u64(b, n, "slot", &v) != 1 || v < VPAD_FIRST_SLOT || v > VPAD_LAST_SLOT) return -1;
    *slot = (int)v;
    return 0;
}

/* The owner token: exactly VPAD_TOKEN_HEX hex characters, kept in lower case. 0, or -1. */
static int token_of(const char *b, size_t n, char tok[VPAD_TOKEN_HEX + 1]) {
    char raw[VPAD_TOKEN_HEX + 2];
    if (webui_json_str(b, n, "token", raw, sizeof raw) != 1 || strlen(raw) != VPAD_TOKEN_HEX) return -1;
    for (int i = 0; i < VPAD_TOKEN_HEX; i++) {
        char c = raw[i];
        if (c >= 'A' && c <= 'F') c = (char)(c - 'A' + 'a');
        if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return -1;
        tok[i] = c;
    }
    tok[VPAD_TOKEN_HEX] = '\0';
    return 0;
}

/* Compares in time independent of where the tokens differ. */
static int token_eq(const char *a, const char *b) {
    unsigned diff = 0;
    for (int i = 0; i < VPAD_TOKEN_HEX; i++) diff |= (unsigned)(a[i] ^ b[i]);
    return diff == 0;
}

static uint32_t token_hash(const char *t) {
    uint32_t h = 2166136261u;
    for (int i = 0; i < VPAD_TOKEN_HEX; i++) h = (h ^ (uint8_t)t[i]) * 16777619u;
    return h;
}

typedef struct {
    int slot;
    char token[VPAD_TOKEN_HEX + 1];
    uint64_t seq;
    vpad_state_t st;
} state_req_t;

/* One vpad.state body (or batch item): 0, or -1 malformed. */
static int parse_state(const char *b, size_t n, state_req_t *r) {
    uint64_t buttons = 0;
    const char *arr;
    size_t alen;
    int t;
    memset(r, 0, sizeof *r);
    vpad_state_neutral(&r->st);
    if (slot_of(b, n, &r->slot) < 0 || token_of(b, n, r->token) < 0 || webui_json_u64(b, n, "seq", &r->seq) != 1)
        return -1;
    if (opt_uint(b, n, "buttons", 0xFFFFFFFFu, &buttons) < 0 || opt_u8(b, n, "lx", &r->st.lx) < 0 ||
        opt_u8(b, n, "ly", &r->st.ly) < 0 || opt_u8(b, n, "rx", &r->st.rx) < 0 || opt_u8(b, n, "ry", &r->st.ry) < 0 ||
        opt_u8(b, n, "l2", &r->st.l2) < 0 || opt_u8(b, n, "r2", &r->st.r2) < 0)
        return -1;
    r->st.buttons = (uint32_t)buttons;
    /* A bad motion block (not three numbers, or one beyond +-1e6) is dropped: the pad keeps the
     * rest of the state, held still. It never refuses the state (or the batch it is in), so one
     * guest's odd sensor cannot stop every other player. */
    if (opt_vec3(b, n, "gyro", r->st.gyro) < 0 || opt_vec3(b, n, "accel", r->st.accel) < 0) {
        memset(r->st.gyro, 0, sizeof r->st.gyro);
        memset(r->st.accel, 0, sizeof r->st.accel);
        r->st.accel[1] = 1.0f;
    }
    t = webui_json_array(b, n, "touch", &arr, &alen);
    if (t < 0 && !webui_json_null(b, n, "touch")) return -1;
    if (t == 1) {
        size_t pos = 0;
        const char *el;
        size_t el_len;
        int k;
        while ((k = array_next(arr, alen, &pos, &el, &el_len)) == 1) {
            uint64_t id = 0, x = 0, y = 0;
            vpad_touch_t *p;
            if (r->st.touch_n >= VPAD_MAX_TOUCH || el[0] != '{') return -1;
            if (opt_uint(el, el_len, "id", 255, &id) < 0 || webui_json_u64(el, el_len, "x", &x) != 1 ||
                webui_json_u64(el, el_len, "y", &y) != 1 || x > 0xFFFF || y > 0xFFFF)
                return -1;
            p = &r->st.touch[r->st.touch_n++];
            p->id = (uint8_t)id;
            p->x = (uint16_t)x;
            p->y = (uint16_t)y;
        }
        if (k < 0) return -1;
    }
    return 0;
}

/* ── Answers ──────────────────────────────────────────────────────────────────────────────────── */

static int answer(size_t cap, int n) {
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

static int refuse(char *out, size_t cap, const char *err) {
    return answer(cap, snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", err));
}

static int refuse_code(char *out, size_t cap, const char *err, int code) {
    return answer(cap, snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\",\"code\":%d}", err, code));
}

/* ── The methods ──────────────────────────────────────────────────────────────────────────────── */

static int opened_answer(char *out, size_t cap, const slot_t *s, int resumed) {
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"slot\":%d,\"handle\":%d,\"user\":%u,\"resumed\":%s}",
                                player_of(s), s->handle, (unsigned)s->user, resumed ? "true" : "false"));
}

/* The same owner opening a live slot again: nothing is made again, and its next state counts
 * whatever its seq (its link may have started over). Lock held. */
static int resume_locked(slot_t *s, long long now_ms, char *out, size_t cap) {
    s->last_alive_ms = now_ms;
    s->have_seq = 0;
    return opened_answer(out, cap, s, 1);
}

int vpad_open_json(const char *body, size_t n, long long now_ms, vpad_user_fn pick_user, char *out, size_t cap) {
    char tok[VPAD_TOKEN_HEX + 1];
    uint8_t desc[VPAD_BLOCK];
    add_req_t r;
    uint64_t user = 0;
    int slot, fd, handle = -1, rc;
    slot_t *s;
    if (!body || n > VPAD_REQ_MAX || slot_of(body, n, &slot) < 0 || token_of(body, n, tok) < 0 ||
        opt_uint(body, n, "user", 0xFFFFFFFFu, &user) < 0)
        return refuse(out, cap, "bad_request");
    s = &g_slots[slot - VPAD_FIRST_SLOT];
    pthread_mutex_lock(&g_lock);
    if (g_stopped) {
        rc = refuse(out, cap, "vpad_stopping");
    } else if (s->open) {
        rc = token_eq(s->token, tok) ? resume_locked(s, now_ms, out, cap) : refuse(out, cap, "vpad_busy");
    } else {
        rc = 0;
    }
    pthread_mutex_unlock(&g_lock);
    if (rc) return rc;

    /* Who the pad is for, outside the lock: the foreground user is a Sony call behind the Sony API
     * lock, which a launch can hold for seconds, and the other players' input must not wait. */
    if (!user) user = pick_user ? pick_user() : vpad_home_user("/user/home");
    if (!user) return refuse(out, cap, "vpad_no_user");

    pthread_mutex_lock(&g_lock);
    if (g_stopped) {
        rc = refuse(out, cap, "vpad_stopping");
        goto done;
    }
    if (s->open) { /* another open of this slot got here first */
        rc = token_eq(s->token, tok) ? resume_locked(s, now_ms, out, cap) : refuse(out, cap, "vpad_busy");
        goto done;
    }
    fd = g_hid->open_hid();
    if (fd < 0) {
        rc = fd == -ENOENT || fd == -ENXIO || fd == -ENODEV ? refuse_code(out, cap, "vpad_unsupported", fd)
                                                            : refuse_code(out, cap, "vpad_failed", fd);
        goto done;
    }
    memset(desc, 0, sizeof desc);
    desc[DESC_CLASS] = 3; /* a standard controller: class 3, subtype 1 */
    desc[DESC_USER] = (uint8_t)user;
    desc[DESC_USER + 1] = (uint8_t)(user >> 8);
    desc[DESC_USER + 2] = (uint8_t)(user >> 16);
    desc[DESC_USER + 3] = (uint8_t)(user >> 24);
    desc[DESC_SUBTYPE] = 1;
    memset(&r, 0, sizeof r);
    r.op = 3;
    r.desc = (uint64_t)(uintptr_t)desc;
    r.handle_out = (uint64_t)(uintptr_t)&handle;
    rc = g_hid->ioctl_hid(fd, VPAD_IOCTL_ADD, &r);
    if (rc != 0 || handle < 0) {
        g_hid->close_hid(fd);
        fprintf(stderr, "[vpad] player %d: the console did not add a pad for user 0x%08x (%d)\n", slot,
                (unsigned)user, rc ? rc : -EIO);
        rc = refuse_code(out, cap, "vpad_failed", rc ? rc : -EIO);
        goto done;
    }
    memset(s, 0, sizeof *s);
    s->open = 1;
    s->fd = fd;
    s->handle = handle;
    s->user = (uint32_t)user;
    memcpy(s->token, tok, sizeof s->token);
    vpad_state_neutral(&s->last);
    s->opened_ms = s->last_live_ms = s->last_alive_ms = s->neutral_ms = now_ms;
    s->last_state_ms = -1;
    s->neutral = 1; /* made at rest */
    if (rest_locked(s) != 0) fprintf(stderr, "[vpad] player %d: first state not taken\n", slot);
    fprintf(stderr, "[vpad] player %d: pad %d made for user 0x%08x\n", slot, handle, (unsigned)user);
    watchdog_ensure_locked();
    rc = opened_answer(out, cap, s, 0);
done:
    pthread_mutex_unlock(&g_lock);
    return rc;
}

/* Applies one state: NULL (with *applied 1, or 0 for an old seq) or the refusal's token (*code an
 * errno for vpad_failed). Lock held. */
static const char *apply_locked(const state_req_t *q, long long now_ms, int *applied, int *code) {
    slot_t *s = &g_slots[q->slot - VPAD_FIRST_SLOT];
    int rc;
    *applied = 0;
    *code = 0;
    if (!s->open) return "vpad_not_open";
    if (!token_eq(s->token, q->token)) return "vpad_not_owner";
    s->last_alive_ms = now_ms;
    if (s->have_seq && q->seq <= s->seq) return NULL;
    rc = insert_locked(s, &q->st);
    if (rc != 0) {
        *code = rc;
        return "vpad_failed";
    }
    s->have_seq = 1;
    s->seq = q->seq;
    s->last = q->st;
    s->last_state_ms = s->last_live_ms = now_ms;
    s->neutral = 0;
    *applied = 1;
    return NULL;
}

int vpad_state_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    state_req_t q;
    const char *err;
    int applied, code;
    if (!body || n > VPAD_REQ_MAX || parse_state(body, n, &q) < 0) return refuse(out, cap, "bad_request");
    pthread_mutex_lock(&g_lock);
    err = apply_locked(&q, now_ms, &applied, &code);
    pthread_mutex_unlock(&g_lock);
    if (err) return code ? refuse_code(out, cap, err, code) : refuse(out, cap, err);
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"applied\":%s}", applied ? "true" : "false"));
}

int vpad_states_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    state_req_t q[VPAD_MAX_BATCH];
    char fails[VPAD_MAX_BATCH * 48 + 1];
    const char *arr, *el;
    size_t alen, el_len, pos = 0, fl = 0;
    int count = 0, k, applied = 0, stale = 0;
    if (!body || n > VPAD_REQ_MAX || webui_json_array(body, n, "items", &arr, &alen) != 1)
        return refuse(out, cap, "bad_request");
    /* A malformed item refuses the whole batch (the engine's bug, not a guest's). */
    while ((k = array_next(arr, alen, &pos, &el, &el_len)) == 1) {
        if (count >= VPAD_MAX_BATCH || el[0] != '{' || parse_state(el, el_len, &q[count]) < 0)
            return refuse(out, cap, "bad_request");
        count++;
    }
    if (k < 0) return refuse(out, cap, "bad_request");
    fails[0] = '\0';
    pthread_mutex_lock(&g_lock);
    for (int i = 0; i < count; i++) {
        int one, code, w;
        const char *err = apply_locked(&q[i], now_ms, &one, &code);
        if (err) {
            w = snprintf(fails + fl, sizeof fails - fl, "%s{\"slot\":%d,\"err\":\"%s\"}", fl ? "," : "", q[i].slot, err);
            if (w > 0 && (size_t)w < sizeof fails - fl)
                fl += (size_t)w;
            else
                fails[fl] = '\0'; /* cannot happen (48 bytes each), but never a cut entry */
        } else if (one) {
            applied++;
        } else {
            stale++;
        }
    }
    pthread_mutex_unlock(&g_lock);
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"applied\":%d,\"stale\":%d,\"failed\":[%s]}", applied, stale,
                                fails));
}

int vpad_hb_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    char tok[VPAD_TOKEN_HEX + 1];
    int slot, live = 0, rc;
    slot_t *s;
    if (!body || n > VPAD_REQ_MAX || slot_of(body, n, &slot) < 0 || token_of(body, n, tok) < 0 ||
        webui_json_bool(body, n, "live", &live) < 0)
        return refuse(out, cap, "bad_request");
    s = &g_slots[slot - VPAD_FIRST_SLOT];
    pthread_mutex_lock(&g_lock);
    if (!s->open) {
        rc = refuse(out, cap, "vpad_not_open");
    } else if (!token_eq(s->token, tok)) {
        rc = refuse(out, cap, "vpad_not_owner");
    } else {
        s->last_alive_ms = now_ms;
        if (live) s->last_live_ms = now_ms;
        rc = answer(cap, snprintf(out, cap, "{\"ok\":true,\"neutral\":%s}", s->neutral ? "true" : "false"));
    }
    pthread_mutex_unlock(&g_lock);
    return rc;
}

int vpad_close_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    char tok[VPAD_TOKEN_HEX + 1];
    int slot, rc;
    slot_t *s;
    (void)now_ms;
    if (!body || n > VPAD_REQ_MAX || slot_of(body, n, &slot) < 0 || token_of(body, n, tok) < 0)
        return refuse(out, cap, "bad_request");
    s = &g_slots[slot - VPAD_FIRST_SLOT];
    pthread_mutex_lock(&g_lock);
    if (s->open && !token_eq(s->token, tok)) {
        rc = refuse(out, cap, "vpad_not_owner");
    } else {
        remove_locked(s, "closed");
        rc = answer(cap, snprintf(out, cap, "{\"ok\":true}"));
    }
    pthread_mutex_unlock(&g_lock);
    return rc;
}

int vpad_output_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    uint8_t flags[4] = {0}, motors[4] = {0}, color[4] = {0}, triggers[24] = {0};
    uint32_t flag_a = 0, flag_b = 0, values[6] = {0};
    output_req_t r;
    int slot, rc;
    slot_t *s;
    (void)now_ms;
    if (!body || n > VPAD_REQ_MAX || slot_of(body, n, &slot) < 0) return refuse(out, cap, "bad_request");
    s = &g_slots[slot - VPAD_FIRST_SLOT];
    memset(&r, 0, sizeof r);
    r.fields[0] = (uint64_t)(uintptr_t)&flag_a;
    r.fields[1] = (uint64_t)(uintptr_t)flags;
    r.fields[2] = (uint64_t)(uintptr_t)motors;
    r.fields[3] = (uint64_t)(uintptr_t)color;
    r.fields[4] = (uint64_t)(uintptr_t)triggers;
    for (int i = 0; i < 6; i++) r.fields[5 + i] = (uint64_t)(uintptr_t)&values[i];
    r.fields[11] = (uint64_t)(uintptr_t)&flag_b;
    pthread_mutex_lock(&g_lock);
    if (!s->open) {
        pthread_mutex_unlock(&g_lock);
        return refuse(out, cap, "vpad_not_open");
    }
    /* Remote Play polls this every 10 ms: it answers at once. */
    r.handle = (uint32_t)s->handle;
    rc = g_hid->ioctl_hid(s->fd, VPAD_IOCTL_OUTPUT, &r);
    pthread_mutex_unlock(&g_lock);
    if (rc != 0) return refuse_code(out, cap, "vpad_failed", rc);
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"slot\":%d,\"motors\":[%u,%u],\"color\":[%u,%u,%u]}", slot,
                                motors[0], motors[1], color[0], color[1], color[2]));
}

int vpad_status_json(long long now_ms, char *out, size_t cap) {
    size_t len;
    int w;
    pthread_mutex_lock(&g_lock);
    w = snprintf(out, cap, "{\"ok\":true,\"slots\":[");
    if (w < 0 || (size_t)w >= cap) goto full;
    len = (size_t)w;
    for (int i = 0; i < VPAD_SLOTS; i++) {
        const slot_t *s = &g_slots[i];
        if (s->open)
            w = snprintf(out + len, cap - len,
                         "%s{\"slot\":%d,\"player\":%d,\"open\":true,\"owner\":\"%08x\",\"user\":%u,\"since_ms\":%lld,"
                         "\"last_state_ms\":%lld,\"neutral\":%s,\"neutral_ms\":%lld}",
                         i ? "," : "", player_of(s), player_of(s), (unsigned)token_hash(s->token), (unsigned)s->user,
                         now_ms - s->opened_ms, s->last_state_ms < 0 ? -1LL : now_ms - s->last_state_ms,
                         s->neutral ? "true" : "false", s->neutral ? now_ms - s->neutral_ms : 0LL);
        else
            w = snprintf(out + len, cap - len, "%s{\"slot\":%d,\"player\":%d,\"open\":false}", i ? "," : "",
                         player_of(s), player_of(s));
        if (w < 0 || (size_t)w >= cap - len) goto full;
        len += (size_t)w;
    }
    w = snprintf(out + len, cap - len, "]}");
    if (w < 0 || (size_t)w >= cap - len) goto full;
    len += (size_t)w;
    pthread_mutex_unlock(&g_lock);
    return (int)len;
full:
    pthread_mutex_unlock(&g_lock);
    return -1;
}
