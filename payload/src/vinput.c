/* The PC's keyboard and mouse as the PS5's own: see include/vinput.h. */
#include "vinput.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "proc_identity.h"
#include "webui_codec.h"

/* ── The built-in backend: a stub ─────────────────────────────────────────────────────────────
 *
 * The console's own devices are relayed to the companion process (src/vinput_relay.c), which
 * runtime.c puts in place of this stub only when it is switched on (vinput_ps5.h: off by default). This stub is what every other helper answers: no devices,
 * "vinput_unsupported". */
static void stub_caps(int *mouse, int *keyboard) {
    *mouse = 0;
    *keyboard = 0;
}
static int stub_mouse_open(void) {
    return VINPUT_E_UNSUPPORTED;
}
static void stub_mouse_close(void) {
}
static int stub_mouse_send(uint8_t buttons, int dx, int dy, int wheel, int hwheel) {
    (void)buttons;
    (void)dx;
    (void)dy;
    (void)wheel;
    (void)hwheel;
    return VINPUT_E_UNSUPPORTED;
}
static int stub_keyboard_open(void) {
    return VINPUT_E_UNSUPPORTED;
}
static void stub_keyboard_close(void) {
}
static int stub_keyboard_send(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    (void)mods;
    (void)keys;
    return VINPUT_E_UNSUPPORTED;
}

static const vinput_backend_t STUB = {
    "stub", stub_caps, stub_mouse_open, stub_mouse_close, stub_mouse_send, stub_keyboard_open, stub_keyboard_close,
    stub_keyboard_send, NULL,
};

/* ── The session ──────────────────────────────────────────────────────────────────────────────── */

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static const vinput_backend_t *g_be = &STUB;
static int g_mouse;     /* the virtual mouse is open */
static int g_keyboard;  /* the virtual keyboard is open */
static uint64_t g_owner; /* the session's owner token, 0 = no session */
static long long g_last_hb;
static uint8_t g_buttons;         /* mouse buttons held, as last sent */
static uint8_t g_mods;            /* keyboard state, as last sent */
static uint8_t g_keys[VINPUT_KEYS];
static const char *g_last_release = "";
static int g_stopped;
static int g_wd_running;
static int g_no_watchdog;

long long vinput_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static const uint8_t NO_KEYS[VINPUT_KEYS];

static void mouse_release_locked(void) {
    if (!g_mouse) return;
    (void)g_be->mouse_send(0, 0, 0, 0, 0); /* let go of every button before the device goes */
    g_be->mouse_close();
    g_mouse = 0;
    g_buttons = 0;
}

static void keyboard_release_locked(void) {
    if (!g_keyboard) return;
    (void)g_be->keyboard_send(0, NO_KEYS);
    g_be->keyboard_close();
    g_keyboard = 0;
    g_mods = 0;
    memset(g_keys, 0, sizeof g_keys);
}

/* Lets go of every held button and key, keeping the devices. */
static void let_go_locked(void) {
    if (g_mouse && g_buttons) {
        (void)g_be->mouse_send(0, 0, 0, 0, 0);
        g_buttons = 0;
    }
    if (g_keyboard && (g_mods || memcmp(g_keys, NO_KEYS, sizeof g_keys) != 0)) {
        (void)g_be->keyboard_send(0, NO_KEYS);
        g_mods = 0;
        memset(g_keys, 0, sizeof g_keys);
    }
}

static void release_locked(const char *why) {
    if (!g_owner && !g_mouse && !g_keyboard) return;
    mouse_release_locked();
    keyboard_release_locked();
    g_owner = 0;
    g_last_release = why;
}

static void *watchdog_main(void *arg) {
    (void)arg;
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    for (;;) {
        usleep(VINPUT_WATCHDOG_STEP_MS * 1000u);
        pthread_mutex_lock(&g_lock);
        if (g_owner && vinput_now_ms() - g_last_hb > VINPUT_HEARTBEAT_TIMEOUT_MS) release_locked("timeout");
        if (!g_owner) {
            g_wd_running = 0;
            pthread_mutex_unlock(&g_lock);
            return NULL;
        }
        pthread_mutex_unlock(&g_lock);
    }
}

/* Runs the watchdog while a session is open (it ends itself once none is). Lock held. */
static void watchdog_ensure_locked(void) {
    pthread_t t;
    pthread_attr_t attr;
    if (g_wd_running || g_no_watchdog) return;
    if (pthread_attr_init(&attr) != 0) return;
    (void)pthread_attr_setstacksize(&attr, 64u * 1024u);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&t, &attr, watchdog_main, NULL) == 0) g_wd_running = 1;
    pthread_attr_destroy(&attr);
    /* No thread: the session still ends on close, on the next request after the timeout (every
     * method checks it first) and when the helper stops. */
}

void vinput_set_backend(const vinput_backend_t *b) {
    pthread_mutex_lock(&g_lock);
    release_locked("backend");
    g_be = b ? b : &STUB;
    g_stopped = 0;
    pthread_mutex_unlock(&g_lock);
}

const vinput_backend_t *vinput_backend(void) {
    return g_be;
}

void vinput_test_no_watchdog(int on) {
    g_no_watchdog = on;
}

/* ── Reports ──────────────────────────────────────────────────────────────────────────────────── */

void vinput_report_decode(const uint8_t raw[VINPUT_REPORT_LEN], vinput_report_t *out) {
    out->buttons = raw[0];
    out->mods = raw[1];
    out->dx = (int16_t)(uint16_t)(raw[2] | (raw[3] << 8));
    out->dy = (int16_t)(uint16_t)(raw[4] | (raw[5] << 8));
    out->wheel = (int8_t)raw[6];
    out->hwheel = (int8_t)raw[7];
    memcpy(out->keys, raw + 8, VINPUT_KEYS);
}

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

int vinput_reports_from_hex(const char *hex, size_t len, vinput_report_t *out, int max) {
    const size_t per = 2u * VINPUT_REPORT_LEN;
    size_t count;
    if (!hex || len == 0 || len % per != 0 || max <= 0) return -1;
    count = len / per;
    if (count > (size_t)max) return -1;
    for (size_t i = 0; i < count; i++) {
        uint8_t raw[VINPUT_REPORT_LEN];
        for (size_t j = 0; j < VINPUT_REPORT_LEN; j++) {
            int hi = hexval(hex[i * per + 2 * j]), lo = hexval(hex[i * per + 2 * j + 1]);
            if (hi < 0 || lo < 0) return -1;
            raw[j] = (uint8_t)(hi << 4 | lo);
        }
        vinput_report_decode(raw, &out[i]);
    }
    return (int)count;
}

/* ── The methods ──────────────────────────────────────────────────────────────────────────────── */

static int answer(size_t cap, int n) {
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

static int session_answer(char *out, size_t cap, int ok, const char *err) {
    return answer(cap,
                  snprintf(out, cap, "{\"ok\":%s,\"open\":%s,\"mouse\":%s,\"keyboard\":%s%s%s%s}",
                           ok ? "true" : "false", g_owner ? "true" : "false", g_mouse ? "true" : "false",
                           g_keyboard ? "true" : "false", err ? ",\"err\":\"" : "", err ? err : "",
                           err ? "\"" : ""));
}

static int refuse(char *out, size_t cap, const char *err) {
    return answer(cap, snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", err));
}

/* A session past its heartbeat is let go before any request looks at it. Lock held. */
static void expire_locked(long long now_ms) {
    if (g_owner && now_ms - g_last_hb > VINPUT_HEARTBEAT_TIMEOUT_MS) release_locked("timeout");
}

int vinput_caps_json(char *out, size_t cap) {
    int mouse = 0, keyboard = 0, n;
    const char *status = NULL, *reason = NULL;
    pthread_mutex_lock(&g_lock);
    expire_locked(vinput_now_ms());
    g_be->caps(&mouse, &keyboard);
    if (g_be->status) status = g_be->status(&reason);
    if (!reason && !mouse && !keyboard) reason = "vinput_unsupported";
    n = snprintf(out, cap, "{\"mouse\":%s,\"keyboard\":%s,\"open\":%s,\"backend\":\"%s\"%s%s%s%s%s%s}",
                 mouse ? "true" : "false", keyboard ? "true" : "false", g_owner ? "true" : "false", g_be->name,
                 status ? ",\"companion\":\"" : "", status ? status : "", status ? "\"" : "",
                 reason ? ",\"reason\":\"" : "", reason ? reason : "", reason ? "\"" : "");
    pthread_mutex_unlock(&g_lock);
    return answer(cap, n);
}

/* Opens (want) or removes (!want) one device. NULL, or the refusal's token. Lock held. */
static const char *device_locked(int want, int can, int *is_open, int (*open_fn)(void), void (*release)(void)) {
    int rc;
    if (!want) {
        release();
        return NULL;
    }
    if (*is_open) return NULL;
    if (!can) return "vinput_unsupported";
    rc = open_fn();
    if (rc == VINPUT_OK) {
        *is_open = 1;
        return NULL;
    }
    return rc == VINPUT_E_UNSUPPORTED ? "vinput_unsupported"
           : rc == VINPUT_E_STARTING  ? "vinput_starting"
           : rc == VINPUT_E_NO_USER   ? "vinput_no_user"
                                      : "vinput_failed";
}

/* While a session is open: why its input waits, when the backend's process is not up (NULL when it
 * is). Lock held. */
static const char *backend_wait_locked(void) {
    const char *reason = NULL, *st;
    if (!g_be->status) return NULL;
    st = g_be->status(&reason);
    if (strcmp(st, "starting") == 0 || strcmp(st, "stopped") == 0) return "vinput_starting";
    if (strcmp(st, "unavailable") == 0) return reason ? reason : "vinput_unsupported";
    return NULL;
}

int vinput_session_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    char op[8];
    uint64_t id = 0;
    int want_mouse = 0, want_keyboard = 0, rc;
    if (!body || n > VINPUT_REQ_MAX || webui_json_str(body, n, "op", op, sizeof op) != 1 ||
        webui_json_u64(body, n, "id", &id) != 1 || id == 0)
        return refuse(out, cap, "bad_request");
    if (webui_json_bool(body, n, "mouse", &want_mouse) < 0 || webui_json_bool(body, n, "keyboard", &want_keyboard) < 0)
        return refuse(out, cap, "bad_request");
    pthread_mutex_lock(&g_lock);
    expire_locked(now_ms);
    if (strcmp(op, "open") == 0) {
        int can_mouse = 0, can_keyboard = 0;
        const char *em, *ek;
        if (g_stopped) {
            rc = session_answer(out, cap, 0, "vinput_stopping");
            goto done;
        }
        if (!want_mouse && !want_keyboard) {
            rc = session_answer(out, cap, 0, "bad_request");
            goto done;
        }
        /* Another owner: its input is let go before this one gets the devices. The same owner
         * opening again starts from nothing held as well: it opens again after an error on its
         * side dropped what it had queued, a key-up among it, and it only sends a report when
         * something changes, so a held key would otherwise stay down. */
        if (g_owner && g_owner != id)
            release_locked("takeover");
        else if (g_owner)
            let_go_locked();
        g_be->caps(&can_mouse, &can_keyboard);
        em = device_locked(want_mouse, can_mouse, &g_mouse, g_be->mouse_open, mouse_release_locked);
        ek = device_locked(want_keyboard, can_keyboard, &g_keyboard, g_be->keyboard_open, keyboard_release_locked);
        if (g_mouse || g_keyboard) {
            g_owner = id;
            g_last_hb = now_ms;
            watchdog_ensure_locked();
        } else {
            g_owner = 0;
        }
        /* Starting outranks the other refusal: the open is asked again once it is up. */
        if ((em && strcmp(em, "vinput_starting") == 0) || (ek && strcmp(ek, "vinput_starting") == 0))
            em = "vinput_starting";
        /* A backend that says why it cannot (no loader for its process, ...) is more use than
         * "unsupported". */
        if (g_be->status && ((em && strcmp(em, "vinput_unsupported") == 0) || (!em && ek && strcmp(ek, "vinput_unsupported") == 0))) {
            const char *why = NULL;
            (void)g_be->status(&why);
            if (why) em = why;
        }
        rc = session_answer(out, cap, !em && !ek, em ? em : ek);
    } else if (strcmp(op, "hb") == 0) {
        if (!g_owner)
            rc = session_answer(out, cap, 0, "vinput_not_open");
        else if (g_owner != id)
            rc = session_answer(out, cap, 0, "vinput_not_owner");
        else {
            g_last_hb = now_ms;
            rc = session_answer(out, cap, 1, backend_wait_locked());
        }
    } else if (strcmp(op, "close") == 0) {
        if (g_owner && g_owner != id)
            rc = session_answer(out, cap, 0, "vinput_not_owner");
        else {
            release_locked("close");
            rc = session_answer(out, cap, 1, NULL);
        }
    } else {
        rc = session_answer(out, cap, 0, "bad_request");
    }
done:
    pthread_mutex_unlock(&g_lock);
    return rc;
}

/* One report to the open devices: the mouse when anything of it changed, the keyboard when its state
 * did. 0, or a backend error. Lock held. */
static int apply_locked(const vinput_report_t *r) {
    int err = 0;
    if (g_mouse && (r->buttons != g_buttons || r->dx || r->dy || r->wheel || r->hwheel)) {
        int rc = g_be->mouse_send(r->buttons, r->dx, r->dy, r->wheel, r->hwheel);
        if (rc == VINPUT_OK)
            g_buttons = r->buttons;
        else
            err = rc;
    }
    if (g_keyboard && (r->mods != g_mods || memcmp(r->keys, g_keys, VINPUT_KEYS) != 0)) {
        int rc = g_be->keyboard_send(r->mods, r->keys);
        if (rc == VINPUT_OK) {
            g_mods = r->mods;
            memcpy(g_keys, r->keys, VINPUT_KEYS);
        } else {
            err = rc;
        }
    }
    return err;
}

int vinput_report_json(const char *body, size_t n, long long now_ms, char *out, size_t cap) {
    char hex[2u * VINPUT_REPORT_LEN * VINPUT_MAX_REPORTS + 1];
    vinput_report_t reps[VINPUT_MAX_REPORTS];
    uint64_t id = 0;
    int count, applied = 0, err = 0, rc;
    if (!body || n > VINPUT_REQ_MAX || webui_json_u64(body, n, "id", &id) != 1 || id == 0 ||
        webui_json_str(body, n, "r", hex, sizeof hex) != 1)
        return refuse(out, cap, "bad_request");
    count = vinput_reports_from_hex(hex, strlen(hex), reps, VINPUT_MAX_REPORTS);
    if (count < 0) return refuse(out, cap, "bad_request");
    pthread_mutex_lock(&g_lock);
    expire_locked(now_ms);
    if (!g_owner) {
        pthread_mutex_unlock(&g_lock);
        return refuse(out, cap, "vinput_not_open");
    }
    if (g_owner != id) {
        pthread_mutex_unlock(&g_lock);
        return refuse(out, cap, "vinput_not_owner");
    }
    g_last_hb = now_ms;
    for (int i = 0; i < count; i++) {
        int e = apply_locked(&reps[i]);
        if (e)
            err = e;
        else
            applied++;
    }
    pthread_mutex_unlock(&g_lock);
    rc = snprintf(out, cap, "{\"ok\":%s,\"applied\":%d%s}", err ? "false" : "true", applied,
                  err == VINPUT_E_UNSUPPORTED ? ",\"err\":\"vinput_unsupported\""
                  : err == VINPUT_E_STARTING  ? ",\"err\":\"vinput_starting\""
                  : err                       ? ",\"err\":\"vinput_failed\""
                                              : "");
    return answer(cap, rc);
}

int vinput_tick(long long now_ms) {
    int let_go;
    pthread_mutex_lock(&g_lock);
    let_go = g_owner && now_ms - g_last_hb > VINPUT_HEARTBEAT_TIMEOUT_MS;
    if (let_go) release_locked("timeout");
    pthread_mutex_unlock(&g_lock);
    return let_go;
}

int vinput_is_open(void) {
    int open;
    pthread_mutex_lock(&g_lock);
    open = g_owner != 0;
    pthread_mutex_unlock(&g_lock);
    return open;
}

void vinput_stop(void) {
    pthread_mutex_lock(&g_lock);
    g_stopped = 1;
    release_locked("stop");
    /* The watchdog sees no session at its next step and ends. */
    pthread_mutex_unlock(&g_lock);
}

const char *vinput_last_release(void) {
    return g_last_release;
}
