/* libSceMbus for the companion's BIND: see include/vinput_mbus.h and include/vinput_wire.h.
 *
 * From ProsperoSunshine (https://github.com/Darkatek7/ProsperoSunshine, PR #33,
 * src/platform/ps5/profile_seat.c, GPL-3.0-only; measured there on FW 10.20): the functions by
 * NID (no SDK stub; checked with the SDK's prospero-nid), sceMbusInit(0, 0) before any of them
 * (without it the bind reads a null pointer), sceMbusGetDeviceInfo_ taking exactly 0x100 bytes
 * with the owner at +0xC8. Written anew here; this file is GPL-3.0-only like its source (see
 * LICENSES/README.md). The probe payload/tools/mbus-probe tries the same calls on their own. */
#include "vinput_mbus.h"

#include <stdint.h>
#include <string.h>
#include <unistd.h>

#include <ps5/kernel.h>

int sceKernelLoadStartModule(const char *path, size_t args, const void *argp, uint32_t flags, const void *option,
                             int *result);

#define NID_INIT "wRPXMGtkOq0" /* sceMbusInit */
#define NID_BIND "Cpo6sQmnZgU" /* sceMbusBindDeviceWithUserId */
#define NID_INFO "RsKVKhSWt28" /* sceMbusGetDeviceInfo_ */
#define INFO_SIZE 0x100
#define INFO_OWNER 0xC8

typedef int (*init_fn)(uint64_t, uint64_t);
typedef int (*bind_fn)(uint64_t device, int32_t user);
typedef int (*info_fn)(uint64_t device, void *info, uint32_t size);

static void (*g_log)(const char *fmt, ...);
static init_fn g_init;
static bind_fn g_bind;
static info_fn g_info;

#define LOG(...)                       \
    do {                               \
        if (g_log) g_log(__VA_ARGS__); \
    } while (0)

static void *resolve(uint32_t module, const char *nid, const char *name) {
    intptr_t p = kernel_dynlib_resolve(getpid(), module, nid);
    /* A failed lookup is 0 or a small number; calling it would end the process. */
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) {
        LOG("%s unresolved", name);
        return NULL;
    }
    return (void *)p;
}

static int m_init(void) {
    int rc = g_init(0, 0);
    LOG("sceMbusInit 0x%x", (unsigned)rc);
    return rc;
}

static int m_bind(uint64_t device, int32_t user) {
    return g_bind(device, user);
}

static int m_owner(uint64_t device, uint32_t *owner) {
    uint8_t info[INFO_SIZE];
    int rc;
    memset(info, 0, sizeof info);
    rc = g_info(device, info, sizeof info);
    if (rc == 0) memcpy(owner, info + INFO_OWNER, sizeof *owner);
    return rc;
}

static const vcomp_mbus_t MBUS = {m_init, m_bind, m_owner, NULL};

const vcomp_mbus_t *vinput_mbus_backend(void (*log)(const char *fmt, ...)) {
    uint32_t module = 0;
    int start = 0, h;
    g_log = log;
    if (kernel_dynlib_handle(getpid(), "libSceMbus.sprx", &module) != 0) {
        h = sceKernelLoadStartModule("/system/common/lib/libSceMbus.sprx", 0, NULL, 0, NULL, &start);
        if (h < 0) {
            LOG("libSceMbus not loaded 0x%x", (unsigned)h);
            return NULL;
        }
        module = (uint32_t)h;
    }
    g_init = (init_fn)resolve(module, NID_INIT, "sceMbusInit");
    g_bind = (bind_fn)resolve(module, NID_BIND, "sceMbusBindDeviceWithUserId");
    g_info = (info_fn)resolve(module, NID_INFO, "sceMbusGetDeviceInfo_");
    return g_init && g_bind && g_info ? &MBUS : NULL;
}
