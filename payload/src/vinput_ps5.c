/* The console's own virtual keyboard and mouse (libSceMouse / libSceKeyboard), as a vinput backend.
 *
 * OFF by default. vinput_ps5_backend() hands it out only when BOTH hold:
 *   - the firmware is on vinput_ps5_firmware_allowed's list (only "13.60": the one the calls were
 *     probed on), and
 *   - it is switched on: built with -DVINPUT_PS5=1, or the file VINPUT_PS5_ENABLE_FILE exists.
 * Otherwise the helper keeps vinput.c's stub (no devices, "vinput_unsupported").
 *
 * What was measured (fat PS5, FW 13.60, a research probe in PS5Term), and what this follows:
 *   - libSceMouse.sprx and libSceKeyboard.sprx load from /system/common/lib with
 *     sceKernelLoadStartModule; their symbols resolve with kernel_dynlib_dlsym (sceKernelDlsym and
 *     libc dlsym fail from a payload: 0x80020003);
 *   - int sceMouseVirtualDeviceAddDevice(const param *p, uint32_t *out_handle) and the keyboard's
 *     twin: param = { int32 user id; uint8 subtype = 0; rest zero } (16 bytes here); returned 0
 *     with a handle, the device auto-assigned to that (foreground) user;
 *   - sceMouseVirtualDeviceInsertData(handle, data[40]): buttons u32 @0x0c, x i32 @0x10, y i32
 *     @0x14 (relative), wheel i32 @0x18, tilt i32 @0x1c, the rest zero;
 *   - sceKeyboardVirtualDeviceInsertData(handle, data): count u32 @0x14 (keys down, <= 16), leds
 *     u32 @0x18, modifiers u32 @0x1c, kind u32 @0x20, u16 keys[16] @0x22 (HID usages); count 0
 *     lets go;
 *   - *DeleteDevice(handle).
 * NOT yet proven: that the events reach a game or app. The button and modifier bits are passed
 * through as HID orders them (Sony's SCE_MOUSE_BUTTON_* / SCE_KEYBOARD_MKEY_* use the same
 * order), and the wheel's sign is the HID one (up is positive); both are unverified.
 *
 * Locking: vinput.c holds its own lock around every call here, one at a time. The foreground
 * user's id is read under sony_api_lock (sceUserService, CE-108262-9). Symbol lookups are kernel
 * reads, under kernel_rw_lock. */
#include "vinput_ps5.h"

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#include <ps5/kernel.h>

#include "kernel_rw_lock.h"
#include "profile.h"
#include "ps5_firmware.h"
#include "sony_api_lock.h"

#ifndef VINPUT_PS5
#define VINPUT_PS5 0
#endif

#define LIBKERNEL_HANDLE 1u
#define LIB_MOUSE "/system/common/lib/libSceMouse.sprx"
#define LIB_KEYBOARD "/system/common/lib/libSceKeyboard.sprx"
/* A device that could not be made is tried again after this long (no user signed in yet, ...). */
#define PROBE_RETRY_MS 10000LL

typedef int (*load_start_fn)(const char *path, size_t argc, const void *argv, uint32_t flags, void *opt, int *res);
typedef int (*add_fn)(const void *param, uint32_t *out_handle);
typedef int (*del_fn)(uint32_t handle);
typedef int (*insert_fn)(uint32_t handle, const void *data);

static pthread_once_t g_once = PTHREAD_ONCE_INIT;
static add_fn m_add, k_add;
static del_fn m_del, k_del;
static insert_fn m_insert, k_insert;

static uint32_t g_mouse, g_keyboard;   /* open handles */
static int g_mouse_open, g_keyboard_open;
/* What AddDevice proved: 1 worked this run, 0 not tried (or failed, retried after probe_at). */
static int g_mouse_ok, g_keyboard_ok;
static long long g_mouse_retry, g_keyboard_retry;

static long long vps5_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void *vps5_sym(uint32_t handle, const char *name) {
    intptr_t p;
    pthread_mutex_lock(&kernel_rw_lock);
    p = kernel_dynlib_dlsym(getpid(), handle, name);
    pthread_mutex_unlock(&kernel_rw_lock);
    /* A failed lookup is a small number or a kernel errno; calling it would take the helper down. */
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) return NULL;
    return (void *)p;
}

/* The module's handle: loaded now, or already in the process. */
static int vps5_module(const char *path, const char *basename, uint32_t *out) {
    load_start_fn load = (load_start_fn)vps5_sym(LIBKERNEL_HANDLE, "sceKernelLoadStartModule");
    int h = load ? load(path, 0, NULL, 0, NULL, NULL) : -1;
    if (h > 0) {
        *out = (uint32_t)h;
        return 0;
    }
    pthread_mutex_lock(&kernel_rw_lock);
    int rc = kernel_dynlib_handle(getpid(), basename, out);
    pthread_mutex_unlock(&kernel_rw_lock);
    if (rc != 0) fprintf(stderr, "[vinput] %s: load 0x%x, not in the process\n", basename, (unsigned)h);
    return rc == 0 ? 0 : -1;
}

static void vps5_resolve_once(void) {
    uint32_t hm = 0, hk = 0;
    if (vps5_module(LIB_MOUSE, "libSceMouse.sprx", &hm) == 0) {
        m_add = (add_fn)vps5_sym(hm, "sceMouseVirtualDeviceAddDevice");
        m_del = (del_fn)vps5_sym(hm, "sceMouseVirtualDeviceDeleteDevice");
        m_insert = (insert_fn)vps5_sym(hm, "sceMouseVirtualDeviceInsertData");
    }
    if (vps5_module(LIB_KEYBOARD, "libSceKeyboard.sprx", &hk) == 0) {
        k_add = (add_fn)vps5_sym(hk, "sceKeyboardVirtualDeviceAddDevice");
        k_del = (del_fn)vps5_sym(hk, "sceKeyboardVirtualDeviceDeleteDevice");
        k_insert = (insert_fn)vps5_sym(hk, "sceKeyboardVirtualDeviceInsertData");
    }
    fprintf(stderr, "[vinput] libSceMouse virtual device %s, libSceKeyboard virtual device %s\n",
            m_add && m_del && m_insert ? "resolved" : "missing", k_add && k_del && k_insert ? "resolved" : "missing");
}

static int vps5_mouse_syms(void) {
    pthread_once(&g_once, vps5_resolve_once);
    return m_add && m_del && m_insert;
}
static int vps5_keyboard_syms(void) {
    pthread_once(&g_once, vps5_resolve_once);
    return k_add && k_del && k_insert;
}

/* AddDevice for the foreground user: 0 with the handle, VINPUT_E_* otherwise. */
static int vps5_add_device(add_fn add, uint32_t *out) {
    uint8_t param[16];
    uint32_t uid;
    int rc;
    pthread_mutex_lock(&sony_api_lock);
    uid = profile_foreground_user(NULL, 0);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
    if (uid == 0) return VINPUT_E_FAILED; /* nobody signed in */
    memset(param, 0, sizeof param);
    memcpy(param, &uid, sizeof uid); /* int32 user id; subtype (byte 4) 0 = mouse/keyboard */
    *out = 0;
    rc = add(param, out);
    if (rc != 0) {
        fprintf(stderr, "[vinput] AddDevice for user 0x%x: 0x%x\n", uid, (unsigned)rc);
        return VINPUT_E_FAILED;
    }
    return VINPUT_OK;
}

/* Is the device there to make? Proven by an AddDevice that worked this run; the first time (or a
 * while after a failure) one is made and removed again to find out. */
static int vps5_probe(add_fn add, del_fn del, int *ok, long long *retry, int already_open) {
    uint32_t h;
    if (*ok || already_open) return 1;
    if (vps5_now_ms() < *retry) return 0;
    if (vps5_add_device(add, &h) == VINPUT_OK) {
        (void)del(h);
        *ok = 1;
        return 1;
    }
    *retry = vps5_now_ms() + PROBE_RETRY_MS;
    return 0;
}

static void ps5_caps(int *mouse, int *keyboard) {
    *mouse = vps5_mouse_syms() && vps5_probe(m_add, m_del, &g_mouse_ok, &g_mouse_retry, g_mouse_open);
    *keyboard = vps5_keyboard_syms() && vps5_probe(k_add, k_del, &g_keyboard_ok, &g_keyboard_retry, g_keyboard_open);
}

static int ps5_mouse_open(void) {
    int rc;
    if (!vps5_mouse_syms()) return VINPUT_E_UNSUPPORTED;
    if (g_mouse_open) return VINPUT_OK;
    rc = vps5_add_device(m_add, &g_mouse);
    if (rc == VINPUT_OK) g_mouse_open = g_mouse_ok = 1;
    return rc;
}

static void ps5_mouse_close(void) {
    if (!g_mouse_open) return;
    (void)m_del(g_mouse);
    g_mouse_open = 0;
}

static void vps5_put32(uint8_t *b, size_t at, uint32_t v) {
    memcpy(b + at, &v, sizeof v);
}

static int ps5_mouse_send(uint8_t buttons, int dx, int dy, int wheel, int hwheel) {
    uint8_t d[40];
    if (!g_mouse_open) return VINPUT_E_FAILED;
    memset(d, 0, sizeof d);
    vps5_put32(d, 0x0c, buttons);
    vps5_put32(d, 0x10, (uint32_t)dx);
    vps5_put32(d, 0x14, (uint32_t)dy);
    vps5_put32(d, 0x18, (uint32_t)wheel);
    vps5_put32(d, 0x1c, (uint32_t)hwheel);
    return m_insert(g_mouse, d) == 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static int ps5_keyboard_open(void) {
    int rc;
    if (!vps5_keyboard_syms()) return VINPUT_E_UNSUPPORTED;
    if (g_keyboard_open) return VINPUT_OK;
    rc = vps5_add_device(k_add, &g_keyboard);
    if (rc == VINPUT_OK) g_keyboard_open = g_keyboard_ok = 1;
    return rc;
}

static void ps5_keyboard_close(void) {
    if (!g_keyboard_open) return;
    (void)k_del(g_keyboard);
    g_keyboard_open = 0;
}

static int ps5_keyboard_send(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    uint8_t d[72];
    uint32_t count = 0;
    if (!g_keyboard_open) return VINPUT_E_FAILED;
    memset(d, 0, sizeof d);
    for (int i = 0; i < VINPUT_KEYS; i++) {
        if (!keys[i]) continue;
        uint16_t k = keys[i];
        memcpy(d + 0x22 + 2 * count, &k, sizeof k);
        count++;
    }
    vps5_put32(d, 0x14, count);
    vps5_put32(d, 0x1c, mods);
    return k_insert(g_keyboard, d) == 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static const vinput_backend_t PS5 = {
    "ps5", ps5_caps, ps5_mouse_open, ps5_mouse_close, ps5_mouse_send, ps5_keyboard_open, ps5_keyboard_close,
    ps5_keyboard_send,
};

const vinput_backend_t *vinput_ps5_backend(void) {
    char kv[256], fw[32];
    size_t len = sizeof kv;
    struct stat st;
    int on = VINPUT_PS5 || stat(VINPUT_PS5_ENABLE_FILE, &st) == 0;
    if (sysctlbyname("kern.version", kv, &len, NULL, 0) != 0) kv[0] = '\0';
    kv[sizeof kv - 1] = '\0';
    ps5_firmware_from_kernel(kv, fw, sizeof fw);
    if (!on || !vinput_ps5_firmware_allowed(fw)) {
        fprintf(stderr, "[vinput] console devices %s (firmware %s): the stub answers\n", on ? "not allowed" : "off",
                fw);
        return NULL;
    }
    fprintf(stderr, "[vinput] console devices on (firmware %s)\n", fw);
    return &PS5;
}
