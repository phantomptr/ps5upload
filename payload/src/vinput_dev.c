/* The console's own virtual keyboard and mouse: see include/vinput_dev.h.
 *
 * What was measured (fat PS5, FW 13.60; PS5Term's research payload research/vdev/vdev.c, `drive`,
 * watched by PS5Term's own reader: every move, click, wheel notch and key arrived, ~8 ms):
 *   - sceUserServiceInitialize(NULL) first; libSceMouse / libSceKeyboard (and libSceMbus,
 *     libScePad, as that payload did) load from /system/common/lib with sceKernelLoadStartModule;
 *     their symbols resolve with kernel_dynlib_dlsym (sceKernelDlsym and dlsym answer 0x80020003);
 *   - sceMouseInit() / sceKeyboardInit() before anything else, or AddDevice answers 0x80df0005 /
 *     0x80da0005 ("not initialised");
 *   - int *VirtualDeviceAddDevice(const param *p, void *out): param = { int32 user id; uint8
 *     subtype 0; rest zero } (32 bytes here); 0 with the device's handle in the first 4 bytes of
 *     out (the kernel writes the result there: a 256-byte buffer is passed, as the test did), the
 *     device bound to that (foreground) user;
 *   - sceMouseVirtualDeviceInsertData(handle, data[40]): buttons u32 @0x0c, x i32 @0x10, y i32
 *     @0x14 (relative), wheel i32 @0x18, tilt i32 @0x1c; buttons as HID (bit0 left, bit1 right);
 *     wheel +1 is up;
 *   - sceKeyboardVirtualDeviceInsertData(handle, data): count u32 @0x14 (keys down, <= 16), leds
 *     u32 @0x18, modifiers u32 @0x1c, kind u16 @0x20 (0), u16 keys[16] @0x22 (HID usages);
 *   - *VirtualDeviceDeleteDevice(handle).
 * NOT proven yet: that a game (rather than PS5Term's reader) takes the events. */
#include "vinput_dev.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include <ps5/kernel.h>

int sceKernelLoadStartModule(const char *path, size_t args, const void *argp, uint32_t flags, const void *option,
                             int *result);
int sceUserServiceInitialize(void *param);
int sceUserServiceGetForegroundUser(int *user);

typedef int (*init_fn)(void);
typedef int (*add_fn)(const void *param, void *out);
typedef int (*del_fn)(int handle);
typedef int (*insert_fn)(int handle, const void *data);

typedef struct {
    int32_t user;
    uint8_t subtype;
    uint8_t reserved[27];
} vdev_param_t;

static void (*g_log)(const char *fmt, ...);
static int g_inited;
static add_fn m_add, k_add;
static del_fn m_del, k_del;
static insert_fn m_insert, k_insert;
static int g_m_ok, g_k_ok; /* Init worked and the symbols are there */
static int g_mh = -1, g_kh = -1;
static int g_user = -1; /* whom the open devices belong to */

#define LOG(...)                          \
    do {                                  \
        if (g_log) g_log(__VA_ARGS__);    \
    } while (0)

static void *sym(int handle, const char *name) {
    intptr_t p = handle > 0 ? kernel_dynlib_dlsym(getpid(), (uint32_t)handle, name) : 0;
    /* A failed lookup is 0 or a small number; calling it would end the process. */
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) {
        LOG("%s unresolved", name);
        return NULL;
    }
    return (void *)p;
}

static int load(const char *name) {
    char path[96];
    int start = 0, h;
    snprintf(path, sizeof path, "/system/common/lib/%s", name);
    h = sceKernelLoadStartModule(path, 0, NULL, 0, NULL, &start);
    if (h < 0) {
        uint32_t have = 0;
        if (kernel_dynlib_handle(getpid(), name, &have) == 0) h = (int)have;
    }
    LOG("load %s 0x%x", name, (unsigned)h);
    return h;
}

void vinput_dev_init(void (*log)(const char *fmt, ...)) {
    int hm, hk, rc, user = -1;
    init_fn m_init, k_init;
    if (g_inited) return;
    g_inited = 1;
    g_log = log;
    rc = sceUserServiceInitialize(NULL);
    LOG("sceUserServiceInitialize 0x%x", (unsigned)rc);
    hm = load("libSceMouse.sprx");
    hk = load("libSceKeyboard.sprx");
    (void)load("libSceMbus.sprx");
    (void)load("libScePad.sprx");
    m_init = (init_fn)sym(hm, "sceMouseInit");
    m_add = (add_fn)sym(hm, "sceMouseVirtualDeviceAddDevice");
    m_del = (del_fn)sym(hm, "sceMouseVirtualDeviceDeleteDevice");
    m_insert = (insert_fn)sym(hm, "sceMouseVirtualDeviceInsertData");
    k_init = (init_fn)sym(hk, "sceKeyboardInit");
    k_add = (add_fn)sym(hk, "sceKeyboardVirtualDeviceAddDevice");
    k_del = (del_fn)sym(hk, "sceKeyboardVirtualDeviceDeleteDevice");
    k_insert = (insert_fn)sym(hk, "sceKeyboardVirtualDeviceInsertData");
    if (m_init && m_add && m_del && m_insert) {
        rc = m_init();
        LOG("sceMouseInit 0x%x", (unsigned)rc);
        g_m_ok = rc == 0;
    }
    if (k_init && k_add && k_del && k_insert) {
        rc = k_init();
        LOG("sceKeyboardInit 0x%x", (unsigned)rc);
        g_k_ok = rc == 0;
    }
    rc = sceUserServiceGetForegroundUser(&user);
    LOG("foreground user 0x%x (0x%x)", (unsigned)user, (unsigned)rc);
}

static int foreground_user(void) {
    int user = -1;
    if (sceUserServiceGetForegroundUser(&user) != 0) return -1;
    return user;
}

/* AddDevice for the foreground user: the handle, or -1. */
static int add_device(add_fn add, const char *what) {
    vdev_param_t p;
    uint8_t out[256];
    int32_t h = -1;
    int rc, user = foreground_user();
    if (user < 0) {
        LOG("%s: nobody signed in", what);
        return -1;
    }
    memset(&p, 0, sizeof p);
    p.user = user;
    memset(out, 0, sizeof out);
    rc = add(&p, out);
    memcpy(&h, out, sizeof h);
    LOG("%s AddDevice(user 0x%x) 0x%x handle %d", what, (unsigned)user, (unsigned)rc, h);
    if (rc != 0 || h < 0) return -1;
    g_user = user;
    return h;
}

static void dev_caps(int *mouse, int *keyboard) {
    *mouse = g_m_ok;
    *keyboard = g_k_ok;
}

static int dev_mouse_open(void) {
    if (!g_m_ok) return VINPUT_E_UNSUPPORTED;
    if (g_mh >= 0) return VINPUT_OK;
    g_mh = add_device(m_add, "mouse");
    return g_mh >= 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static void dev_mouse_close(void) {
    if (g_mh < 0) return;
    LOG("mouse DeleteDevice 0x%x", (unsigned)m_del(g_mh));
    g_mh = -1;
}

static int dev_mouse_send(uint8_t buttons, int dx, int dy, int wheel, int hwheel) {
    uint8_t d[40];
    uint64_t ts;
    struct timespec t;
    if (g_mh < 0) return VINPUT_E_FAILED;
    clock_gettime(CLOCK_MONOTONIC, &t);
    ts = (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
    memset(d, 0, sizeof d);
    memcpy(d, &ts, sizeof ts);
    d[8] = 1; /* connected */
    memcpy(d + 0x0c, &(uint32_t){buttons}, 4);
    memcpy(d + 0x10, &(int32_t){dx}, 4);
    memcpy(d + 0x14, &(int32_t){dy}, 4);
    memcpy(d + 0x18, &(int32_t){wheel}, 4);
    memcpy(d + 0x1c, &(int32_t){hwheel}, 4);
    return m_insert(g_mh, d) == 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static int dev_keyboard_open(void) {
    if (!g_k_ok) return VINPUT_E_UNSUPPORTED;
    if (g_kh >= 0) return VINPUT_OK;
    g_kh = add_device(k_add, "keyboard");
    return g_kh >= 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static void dev_keyboard_close(void) {
    if (g_kh < 0) return;
    LOG("keyboard DeleteDevice 0x%x", (unsigned)k_del(g_kh));
    g_kh = -1;
}

static int dev_keyboard_send(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    uint8_t d[96];
    uint32_t count = 0;
    uint64_t ts;
    struct timespec t;
    if (g_kh < 0) return VINPUT_E_FAILED;
    clock_gettime(CLOCK_MONOTONIC, &t);
    ts = (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
    memset(d, 0, sizeof d);
    memcpy(d, &ts, sizeof ts);
    for (int i = 0; i < VINPUT_KEYS; i++) {
        uint16_t k = keys[i];
        if (!k) continue;
        memcpy(d + 0x22 + 2 * count, &k, sizeof k);
        count++;
    }
    memcpy(d + 0x14, &count, 4);
    memcpy(d + 0x1c, &(uint32_t){mods}, 4);
    return k_insert(g_kh, d) == 0 ? VINPUT_OK : VINPUT_E_FAILED;
}

static const vinput_backend_t DEV = {
    "ps5", dev_caps, dev_mouse_open, dev_mouse_close, dev_mouse_send, dev_keyboard_open, dev_keyboard_close,
    dev_keyboard_send, NULL,
};

const vinput_backend_t *vinput_dev_backend(void) {
    return &DEV;
}

int vinput_dev_tick(void) {
    int user, had_m = g_mh >= 0, had_k = g_kh >= 0;
    static const uint8_t none[VINPUT_KEYS];
    if (!had_m && !had_k) return 0;
    user = foreground_user();
    if (user < 0 || user == g_user) return 0;
    LOG("foreground user 0x%x -> 0x%x: devices made again", (unsigned)g_user, (unsigned)user);
    if (had_m) {
        (void)dev_mouse_send(0, 0, 0, 0, 0);
        dev_mouse_close();
        (void)dev_mouse_open();
    }
    if (had_k) {
        (void)dev_keyboard_send(0, none);
        dev_keyboard_close();
        (void)dev_keyboard_open();
    }
    return 1;
}
