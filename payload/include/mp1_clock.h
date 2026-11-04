#ifndef PS5UPLOAD_MP1_CLOCK_H
#define PS5UPLOAD_MP1_CLOCK_H

#include <stddef.h>
#include <stdint.h>

/* GPU (GFX) clock and per-core CPU clock, read from the SoC's power controller (the SMU behind
 * the stock /dev/mp1 driver). READ-ONLY: this file knows exactly two requests and refuses any
 * other. The ioctl numbers and field meanings come from NeoEnabler (github.com/gasb07/NeoEnabler,
 * src/gpu_control.h and src/cpu_control.h); its users ran them on FW 13.20 and 13.60 only.
 *
 *   0x406CAE01  IOC_OUT, 108 bytes: a u32[27] table; word 21 = current GFX clock in MHz.
 *               psdevwiki lists 0x4068AE01 (104 bytes) instead, so the table is passed a
 *               zeroed 256-byte buffer: the kernel copies out the size encoded in the request
 *               (108), and the spare words must stay zero. A driver that writes past them is
 *               not the one this was written for, so that also marks it unsupported.
 *   0xC00CAE1E  IOC_INOUT, 12 bytes: {u32 core; u32 pstate; u32 live_mhz}. `core` (0..7) goes
 *               in, pstate (0 = fastest) and the core's clock in MHz come back. The clock is the
 *               SMU's figure for the core's P-state, not a cycle count.
 *
 * The same driver also has requests that CHANGE clocks (the GFX clock set, the mode switch and
 * the core P-state set). This helper must never issue them: payload/tools/check-mp1-readonly.sh
 * fails the build if their numbers or names appear anywhere in payload/.
 *
 * Safety: one mutex around every /dev/mp1 access, a 2 s cache of the last result, and a latch:
 * the first time the driver refuses (no such device or request, not permitted, a wrong size) it is
 * marked unsupported for the rest of the helper's life and never asked again. "Not now" is not a
 * refusal (mp1_errno_transient): the SMU busy while a game runs must not turn the clocks off for the
 * session, so that keeps the last values and the next read tries again. Values outside a plausible
 * range read as 0 = unknown. The SMU can take a while to answer, so this runs only for an explicit
 * "Read sensors" request, never a timer. */

#define MP1_GFX_IOCTL_GET   0x406CAE01UL
#define MP1_CORE_IOCTL_GET  0xC00CAE1EUL

#define MP1_CORES           8
#define MP1_GFX_WORDS       27 /* 108 bytes = IOCPARM_LEN(MP1_GFX_IOCTL_GET) */
#define MP1_GFX_BUF_WORDS   64 /* the 256-byte buffer actually passed */
#define MP1_GFX_MHZ_WORD    21
#define MP1_GPU_MIN_MHZ     100
#define MP1_GPU_MAX_MHZ     2500
#define MP1_CORE_MIN_MHZ    400
#define MP1_CORE_MAX_MHZ    4000
#define MP1_CORE_PSTATE_MAX 63 /* above = not reported */
#define MP1_CACHE_MS        2000

/* How /dev/mp1 is reached; the helper passes the real calls (hw_info.c), the host test a fake.
 * open_ro: an fd, or -1 with errno set. get: one read request on that fd, 0 on success. */
typedef struct {
    int (*open_ro)(void *ctx);
    int (*get)(void *ctx, int fd, unsigned long req, void *buf);
    void (*close_fd)(void *ctx, int fd);
    int64_t (*now_ms)(void *ctx); /* monotonic */
    void *ctx;
} mp1_ops_t;

enum {
    MP1_OK = 0,
    MP1_UNSUPPORTED = -1, /* latched: the driver refused once, nothing more is tried */
};

typedef struct {
    int state;                        /* MP1_OK / MP1_UNSUPPORTED */
    uint32_t gpu_mhz;                 /* 0 = unknown */
    uint32_t core_mhz[MP1_CORES];     /* 0 = unknown */
    uint32_t core_pstate[MP1_CORES];  /* 0xFFFFFFFF = not reported */
    uint32_t core_raw_mhz[MP1_CORES]; /* live_mhz as returned, unfiltered (debug) */
    uint32_t gfx_raw[MP1_GFX_WORDS];  /* the table as read, for mapping the other words later */
} mp1_clocks_t;

/* Fill *out (zeroed first): from the 2 s cache, else one GFX read and eight core reads on one fd
 * under the lock. MP1_OK, also with the last values (or zeros) when the driver said "not now", or
 * MP1_UNSUPPORTED (all zero) once an open/ioctl has been refused. */
int mp1_clocks_read(const mp1_ops_t *ops, mp1_clocks_t *out);

/* Whether a failed open or ioctl's errno means "not now" rather than a refusal. */
int mp1_errno_transient(int err);

/* Word 21 of a GFX table when it is in MP1_GPU_MIN_MHZ..MP1_GPU_MAX_MHZ, else 0. */
uint32_t mp1_gpu_mhz_from_table(const uint32_t words[MP1_GFX_WORDS]);
/* `mhz` when in MP1_CORE_MIN_MHZ..MP1_CORE_MAX_MHZ, else 0. */
uint32_t mp1_core_mhz_valid(uint32_t mhz);

/* The HW_TEMPS lines for a read: gpu_clock_src, cpu_core_mhz (8 values), cpu_core_mhz_min,
 * cpu_core_mhz_max, and with `debug` mp1_clk_raw (27 hex words) and mp1_core_raw
 * (pstate:mhz per core). snprintf's return. */
int mp1_clocks_format(const mp1_clocks_t *c, int debug, char *out, size_t cap);

/* Host tests only: forget the cache and the latch. */
void mp1_clocks_reset(void);

#endif /* PS5UPLOAD_MP1_CLOCK_H */
