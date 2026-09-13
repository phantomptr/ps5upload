/*
 * Hardware monitoring for ps5upload.
 *
 * Uses Sony kernel APIs:
 *   sceKernelGetHwModelName, sceKernelGetHwSerialNumber
 *   sceKernelGetCpuTemperature, sceKernelGetSocSensorTemperature
 *   sceKernelGetCpuFrequency, sceKernelGetDirectMemorySize
 *   sceKernelGetSocPowerConsumption (throttled to 5s between reads)
 *
 * All resolved via dlopen(libkernel_web.sprx) lazily; on a firmware
 * where any symbol is missing, the affected field reads as 0 and
 * the rest of the output is still valid.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <pthread.h>
#include <stdatomic.h>
#include <time.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <dlfcn.h>
#include <setjmp.h>
#include <signal.h>
#include <string.h>

#include <ps5/kernel.h>

#include "hw_info.h"
#include "hw_guard.h"
#include "shellui_rpc.h"
#include "config.h"

/* ── Fault guard for Sony hardware getters ────────────────────────
 *
 * See hw_guard.h for the why. Per-thread state: the mgmt model runs one
 * thread per connection, and several can read hardware concurrently, so the
 * jump buffer + armed flag must be thread-local (a shared buffer would let
 * one thread's siglongjmp resume another thread's stack = instant corruption).
 */
static __thread sigjmp_buf            g_hwg_jmp;
static __thread volatile sig_atomic_t g_hwg_armed = 0;
static __thread const char           *g_hwg_call = "";
static __thread size_t                g_hwg_call_len = 0;

int hw_guard_try_recover(int sig) {
    if (!g_hwg_armed) return 0;
    /* Only recover from genuine memory-fault signals; let everything else
     * (SIGTERM/SIGHUP/SIGABRT) take the normal fatal path. */
    if (sig != SIGSEGV && sig != SIGBUS && sig != SIGILL) return 0;
    g_hwg_armed = 0;
    /* Async-signal-safe breadcrumb only — fprintf is NOT safe in a signal
     * handler (stdio lock). write(2) with a pre-stored length is. This line
     * lands in the payload's captured stderr / PS5 log so the faulting getter
     * is named even when no debugger is attached. */
    static const char pfx[] = "[hw_info] FAULT in Sony getter: ";
    static const char sfx[] = " (skipped, reported unavailable)\n";
    (void)write(2, pfx, sizeof(pfx) - 1);
    if (g_hwg_call && g_hwg_call_len) (void)write(2, g_hwg_call, g_hwg_call_len);
    (void)write(2, sfx, sizeof(sfx) - 1);
    siglongjmp(g_hwg_jmp, sig);
    return 1; /* unreachable — siglongjmp does not return */
}

/* Run `stmt` (a Sony hw getter call) under the fault guard. `label` MUST be a
 * string literal (used for both the breadcrumb and sizeof-length). Emits an
 * always-on breadcrumb BEFORE the call (so a hang — not just a fault — leaves
 * the last-attempted getter in the log), then arms the per-thread jump. On a
 * fault inside `stmt`, control resumes at the sigsetjmp with a non-zero
 * return and `stmt`'s effects are skipped — callers must have a safe default
 * already in place and should read results through a `volatile` so the value
 * is well-defined on the longjmp path. */
#define HW_GUARD(label, stmt)                                              \
    do {                                                                   \
        fprintf(stderr, "[hw_info] -> %s\n", (label));                     \
        fflush(stderr);                                                    \
        g_hwg_call = (label);                                              \
        g_hwg_call_len = sizeof(label) - 1;                                \
        if (sigsetjmp(g_hwg_jmp, 1) == 0) {                                \
            g_hwg_armed = 1;                                               \
            stmt;                                                          \
            g_hwg_armed = 0;                                               \
        } else {                                                           \
            g_hwg_armed = 0;                                               \
        }                                                                  \
    } while (0)

/* ── Fan control (/dev/icc_fan) ──────────────────────────────────
 *
 * The /dev/icc_fan ioctl 0xC01C8F07 is the canonical fan-threshold
 * setter used across the PS5 homebrew ecosystem. The magic code
 * encodes:
 *   0xC01C8F07 = _IOC(IOC_INOUT, 'Å', 0x07, 28)   // 'Å' (0x8F) = ICC group
 * The trailing 28 is IOCPARM_LEN: because the direction is IOC_INOUT,
 * FreeBSD's generic ioctl path copies 28 bytes IN from the userspace
 * buffer and 28 bytes back OUT into it — so the command buffer MUST be
 * 28 bytes even though only the threshold at offset 5 matters. A smaller
 * buffer (we shipped 10) takes an 18-byte stack overflow on the copyout.
 *
 * Why the O_RDONLY open flag with a WRITE ioctl: FreeBSD's ioctl
 * permission check allows RDONLY fds for IOC_INOUT commands. Both
 * reference implementations open RDONLY — keeping the same flag
 * here means if Sony ever tightens the check (e.g., requires RDWR
 * on future firmware), our behavior will fail the same way theirs
 * does, and we can match their eventual fix. */
#define ICC_FAN_IOCTL_SET_THRESHOLD 0xC01C8F07UL
#define ICC_FAN_DEVICE_NODE         "/dev/icc_fan"
#define ICC_FAN_CMD_LEN             28  /* = IOCPARM_LEN(0xC01C8F07); kernel copies in/out this many bytes */
#define ICC_FAN_THRESHOLD_OFFSET    5

/* PS5 firmware resets the fan threshold to its stock value on every
 * app/game launch (and on suspend/resume + some system-menu paths).
 * Without auto-reapply, the user's "65 °C threshold" setting silently
 * reverts the moment they boot a game — exactly the workload where
 * the lower threshold matters. A background thread that re-issues the
 * pinned value every 15 s defeats the reset cheaply; sonicloader's
 * fan daemon uses the same interval and it's been stable in the wild.
 *
 * Why 15 s and not faster: the firmware reset happens once per launch
 * transition (not continuously), so any tick smaller than the
 * user-perceptible delay between launch and "fan ramps up" is enough.
 * Going below 5 s would just burn extra ioctls for no thermal gain.
 *
 * The interval is user-configurable via hw_fan_set_reapply_interval()
 * (persisted to fan_reapply.conf). Default is 15 s if no persisted
 * file exists. Clamped to [1, 300] so a misconfigured value can't
 * disable the watcher (0 would spin-loop) or make it effectively
 * dormant (>300 s risks a game-launch reset going uncountered). */
#define FAN_REAPPLY_DEFAULT_SEC  15
/* Physical bounds for the SoC power rails. A single rail drawing over 200 W,
 * or a total over 500 W, is not a real reading on this hardware — report 0
 * ("unavailable") rather than a fabricated number. */
#define HW_POWER_RAIL_MAX_MW   200000u
#define HW_POWER_TOTAL_MAX_MW  500000u

#define FAN_REAPPLY_MIN_SEC       1
#define FAN_REAPPLY_MAX_SEC     300

/* ── Sony kernel function pointers ───────────────────────────────
 *
 * Resolved via `dlsym(RTLD_DEFAULT, ...)` rather than dlopen of a
 * specific .sprx. RTLD_DEFAULT searches every library already loaded
 * into the process. This matters because Sony's kernel-APIs aren't
 * all in libkernel.sprx -- hw_info (ModelName, Serial, DirectMem) is
 * in libkernel, but temp/freq/power APIs live in libSceSystemService
 * or libkernel_sys, which prospero-clang auto-links. A single
 * RTLD_DEFAULT call picks up whichever library actually exports each
 * symbol, while `dlopen("/system/common/lib/libkernel.sprx")` was
 * only finding a subset AND returning garbage addresses for the ones
 * it didn't export -- calling those crashed the payload.
 *
 * An alternative approach would use extern declarations + let
 * prospero-clang's -lkernel_web do the resolution. Our RTLD_DEFAULT
 * path is slightly more expensive per call (first call
 * does the cross-library search) but preserves the "graceful degrade
 * on missing symbol" behavior we need for firmware compat. */

typedef int     (*sceKernelGetHwModelName_fn)(char *name);
typedef int     (*sceKernelGetHwSerialNumber_fn)(char *serial);
typedef int     (*sceKernelGetCpuTemperature_fn)(int *temperature);
typedef int     (*sceKernelGetSocSensorTemperature_fn)(int sensor_id, int *temperature);
typedef long    (*sceKernelGetCpuFrequency_fn)(void);
typedef size_t  (*sceKernelGetDirectMemorySize_fn)(void);
/* ABI: int sceKernelGetSocPowerConsumption(void *out_raw) — ONE argument,
 * and it writes a 0x70-byte (112) sample block. Cross-checked against
 * drakmor/ps5-hwinfo, which reads it successfully and decodes the layout.
 *
 * This was declared wrong twice, each time a stack overflow on the caller:
 *   (uint32_t *)                 -> kernel writes 112 bytes into 4   (-108)
 *   (uint64_t *, double)         -> kernel writes 112 bytes into 8   (-104)
 * The second was committed as "the corrected ABI"; it was not. It moved the
 * overflow from 108 bytes to 104 and kept a trailing `double` the function
 * does not take. Smashing ~100 bytes of the stack frame on the management
 * request thread is the "hang/disconnect on FW 9.60 Pro" we kept blaming on
 * the API — undefined behaviour lands on different things per build and per
 * firmware, which is exactly why it looked firmware-specific and why some
 * consoles survived it.
 *
 * The destination MUST be at least sizeof(soc_power_sample_t). Never pass a
 * scalar. */
typedef int     (*sceKernelGetSocPowerConsumption_fn)(void *out_raw);

/* The 0x70-byte block the call fills: 8 power rails of
 * {power_mW, voltage_mV, current_mA} as uint32, then auxiliary data from
 * index 24 on. Values are ALREADY in milli-units — there is nothing to
 * guess or normalise. Rail order (per the reference): GPU Core, GPU IO,
 * CPU + SoC, CPU IO, GDDR6 Ch 0-1, Ch 2-3, Ch 4-5, Ch 6-7. */
#define SOC_POWER_SAMPLE_BYTES  0x70
#define SOC_POWER_RAILS         8
#define SOC_POWER_FIELDS        3   /* power_mW, voltage_mV, current_mA */
typedef union {
    uint8_t  bytes[SOC_POWER_SAMPLE_BYTES];
    uint32_t u32[SOC_POWER_SAMPLE_BYTES / sizeof(uint32_t)];
    uint64_t u64[SOC_POWER_SAMPLE_BYTES / sizeof(uint64_t)];
} soc_power_sample_t;
/* Extra telemetry getters, ABI per the Elf Arsenal reference impl. All
 * three are resolved lazily and called ONLY from the on-demand HW_TEMPS
 * path (never the always-on HW_INFO poll), so a wedge on an untested SKU
 * stays a recoverable, user-triggered event rather than dropping the
 * helper on tab-open. */
typedef int     (*sceKernelGetBasicProductShape_fn)(int *out);
typedef int     (*sceKernelGetCpuUsageAll_fn)(int *per_core_pct, int *count_out);
typedef int     (*sceKernelGetCurrentFanDuty_fn)(uint16_t *out_duty, void *scratch);

typedef struct {
    int   resolved;
    sceKernelGetHwModelName_fn           model_name;
    sceKernelGetHwSerialNumber_fn        serial;
    sceKernelGetCpuTemperature_fn        cpu_temp;
    sceKernelGetSocSensorTemperature_fn  soc_temp;
    sceKernelGetCpuFrequency_fn          cpu_freq;
    sceKernelGetDirectMemorySize_fn      direct_mem;
    sceKernelGetSocPowerConsumption_fn   soc_power;
    sceKernelGetBasicProductShape_fn     product_shape;
    sceKernelGetCpuUsageAll_fn           cpu_usage;
    sceKernelGetCurrentFanDuty_fn        fan_duty;
} hw_syms_t;

static hw_syms_t g_hw = {0};
static pthread_once_t g_hw_once = PTHREAD_ONCE_INIT;

static void hw_resolve_impl(void) {
    /* RTLD_DEFAULT searches every already-loaded library in the
     * process. Any symbol not exported anywhere stays NULL and the
     * affected field reads as 0 in the output -- handlers all guard
     * the call site with `if (g_hw.fn)`. */
    dlerror();
    g_hw.model_name  = (sceKernelGetHwModelName_fn)          dlsym(RTLD_DEFAULT, "sceKernelGetHwModelName");
    g_hw.serial      = (sceKernelGetHwSerialNumber_fn)       dlsym(RTLD_DEFAULT, "sceKernelGetHwSerialNumber");
    g_hw.cpu_temp    = (sceKernelGetCpuTemperature_fn)       dlsym(RTLD_DEFAULT, "sceKernelGetCpuTemperature");
    g_hw.soc_temp    = (sceKernelGetSocSensorTemperature_fn) dlsym(RTLD_DEFAULT, "sceKernelGetSocSensorTemperature");
    g_hw.cpu_freq    = (sceKernelGetCpuFrequency_fn)         dlsym(RTLD_DEFAULT, "sceKernelGetCpuFrequency");
    g_hw.direct_mem  = (sceKernelGetDirectMemorySize_fn)     dlsym(RTLD_DEFAULT, "sceKernelGetDirectMemorySize");
    g_hw.soc_power   = (sceKernelGetSocPowerConsumption_fn)  dlsym(RTLD_DEFAULT, "sceKernelGetSocPowerConsumption");
    g_hw.product_shape = (sceKernelGetBasicProductShape_fn)  dlsym(RTLD_DEFAULT, "sceKernelGetBasicProductShape");
    g_hw.cpu_usage   = (sceKernelGetCpuUsageAll_fn)          dlsym(RTLD_DEFAULT, "sceKernelGetCpuUsageAll");
    g_hw.fan_duty    = (sceKernelGetCurrentFanDuty_fn)       dlsym(RTLD_DEFAULT, "sceKernelGetCurrentFanDuty");
    (void)dlerror();
    g_hw.resolved = 1;
}

static void hw_resolve_once(void) {
    pthread_once(&g_hw_once, hw_resolve_impl);
}

/* ── sysctl helpers (no Sony APIs required) ──────────────────────── */

static int sysctl_string(const char *name, char *out, size_t out_cap) {
    size_t sz = out_cap;
    if (sysctlbyname(name, out, &sz, NULL, 0) != 0) {
        out[0] = '\0';
        return -1;
    }
    if (sz < out_cap) out[sz] = '\0';
    else out[out_cap - 1] = '\0';
    return 0;
}

static int sysctl_int(const char *name, int *out) {
    size_t sz = sizeof(*out);
    if (sysctlbyname(name, out, &sz, NULL, 0) != 0) {
        *out = 0;
        return -1;
    }
    return 0;
}

static int sysctl_uint64(const char *name, uint64_t *out) {
    size_t sz = sizeof(*out);
    if (sysctlbyname(name, out, &sz, NULL, 0) != 0) {
        *out = 0;
        return -1;
    }
    return 0;
}

/* ── HW_INFO: static info, read once and cached forever ──────────── */

static pthread_mutex_t g_hwinfo_lock = PTHREAD_MUTEX_INITIALIZER;
static char            g_hwinfo_buf[2048];
static size_t          g_hwinfo_len = 0;
static int             g_hwinfo_valid = 0;

int hw_info_get_text(char *out, size_t out_cap, size_t *out_written,
                     const char **err_reason_out) {
    if (!out || out_cap < 256) {
        if (err_reason_out) *err_reason_out = "hw_info_buffer_too_small";
        return -1;
    }
    hw_resolve_once();
    pthread_mutex_lock(&g_hwinfo_lock);
    if (!g_hwinfo_valid) {
        char model_name[1024] = {0};
        char serial[1024]     = {0};
        char hw_machine[256]  = {0};
        char ostype[64]       = {0};
        char osrelease[64]    = {0};
        int  ncpu             = 0;

        /* model + serial come from dlsym'd Sony getters that can FAULT in
         * some loader/host-process contexts (see hw_guard.h) — guard each so
         * a fault degrades the field to a default instead of dropping the
         * helper. `volatile` so the result is well-defined on the longjmp
         * (fault) path; the default is already set, so a fault → fallback. */
        volatile int model_ok = 0;
        if (g_hw.model_name) {
            HW_GUARD("sceKernelGetHwModelName",
                     model_ok = (g_hw.model_name(model_name) == 0));
        }
        if (!model_ok || model_name[0] == '\0') {
            snprintf(model_name, sizeof(model_name), "%s", "PlayStation 5");
        }
        volatile int serial_ok = 0;
        if (g_hw.serial) {
            HW_GUARD("sceKernelGetHwSerialNumber",
                     serial_ok = (g_hw.serial(serial) == 0));
        }
        if (!serial_ok || serial[0] == '\0') {
            snprintf(serial, sizeof(serial), "%s", "N/A");
        }
        sysctl_string("hw.machine",     hw_machine, sizeof(hw_machine));
        sysctl_string("kern.ostype",    ostype,     sizeof(ostype));
        sysctl_string("kern.osrelease", osrelease,  sizeof(osrelease));
        sysctl_int("hw.ncpu", &ncpu);

        /* Physical RAM. Detection chain ordered to avoid kernel log noise:
         *   1. sceKernelGetDirectMemorySize — PS5-specific, most accurate.
         *   2. page-count math (vm.stats.vm.v_page_count × hw.pagesize) —
         *      APPROVED sysctls on PS5; the product is exact.
         *   3. 16 GiB default (every PS5 has this).
         * Deliberately does NOT query hw.physmem / hw.realmem / hw.usermem:
         * those are FreeBSD-generic but UNAPPROVED on PS5, and each call spams
         * the kernel log with "[SYSCTL] Error : hw.physmem is not approved.
         * Please report to PPRBUG-6864." They bought nothing the page-count
         * product doesn't already give, so they're gone. */
        uint64_t physmem = 0;
        if (g_hw.direct_mem) {
            /* volatile temp so the value is well-defined on the fault path. */
            volatile uint64_t dm = 0;
            HW_GUARD("sceKernelGetDirectMemorySize",
                     dm = (uint64_t)g_hw.direct_mem());
            physmem = dm;
        }
        if (physmem == 0) {
            int pagesize = 0;
            uint64_t page_count = 0;
            sysctl_int("hw.pagesize", &pagesize);
            if (sysctl_uint64("vm.stats.vm.v_page_count", &page_count) == 0 &&
                pagesize > 0 && page_count > 0) {
                physmem = page_count * (uint64_t)pagesize;
            }
        }
        if (physmem == 0) physmem = 16ULL * 1024 * 1024 * 1024;

        /* Precise firmware word via the SDK's kernel-R/W helper
         * (kernel_get_fw_version → KERNEL_ADDRESS_DATA_BASE + a FW-specific
         * offset). This is the ONLY kernel-memory read on the Hardware-tab
         * path, and a read at a wrong/unmapped offset faults hard and kills
         * the helper — unlike a Sony-API miss, which just returns an error.
         *
         * Symptom when it goes wrong: on PS5 Slim (CFI-2000) the Hardware
         * tab dropped the payload the instant it loaded. The disconnect fix
         * was validated on Pro 9.60 + phat 5.10, but the Slim is a different
         * SKU/FW the SDK's data-base offset isn't proven against.
         *
         * 2.27.x — re-enabled as an explicit, GUARDED opt-in:
         *   - DEFAULT: kfw stays 0, identical to the previously-disabled
         *     behaviour. The desktop derives the FW family from the
         *     kern.version string when this word is 0 (its documented
         *     fallback), so nothing regresses.
         *   - Set PS5UPLOAD_PRECISE_FW=1 in the payload's environment to
         *     attempt the precise read. hw_info is built once and cached
         *     forever, so even when enabled the kernel read runs AT MOST
         *     ONCE per payload lifetime — never on a repeated/auto poll.
         *
         * Enable ONLY on a SKU/FW whose SDK offset you've validated — on an
         * unproven target it can still crash the helper. Intended for
         * FW-12.xx point-release triage (e.g. telling 12.00 from 12.40 while
         * diagnosing the install-launch bug), not for general use. */
        uint32_t kfw = 0;
        {
            const char *precise = getenv("PS5UPLOAD_PRECISE_FW");
            if (precise != NULL && precise[0] == '1' && precise[1] == '\0') {
                /* Guarded: a kernel read at a wrong/unmapped offset faults —
                 * the original Slim crash. With the guard a fault degrades to
                 * kfw=0 (string-parse fallback) instead of dropping the
                 * helper. volatile temp for the longjmp path. */
                volatile uint32_t fw = 0;
                HW_GUARD("kernel_get_fw_version", fw = kernel_get_fw_version());
                kfw = fw;
                fprintf(stderr,
                        "[hw_info] PS5UPLOAD_PRECISE_FW=1 — "
                        "kernel_get_fw_version()=0x%08X\n",
                        (unsigned)kfw);
            }
        }

        int n = snprintf(g_hwinfo_buf, sizeof(g_hwinfo_buf),
            "model=%s\n"
            "serial=%s\n"
            "has_wlan_bt=1\n"
            "has_optical_out=0\n"
            "hw_model=%s\n"
            "hw_machine=%s\n"
            "os=%s %s\n"
            "ncpu=%d\n"
            "physmem=%llu\n"
            "kernel_fw_version=%u\n",
            model_name, serial, model_name, hw_machine,
            ostype, osrelease, ncpu, (unsigned long long)physmem,
            (unsigned)kfw);
        if (n < 0) n = 0;
        if ((size_t)n >= sizeof(g_hwinfo_buf)) n = sizeof(g_hwinfo_buf) - 1;
        g_hwinfo_len = (size_t)n;
        g_hwinfo_valid = 1;
    }

    size_t n = g_hwinfo_len;
    if (n > out_cap) n = out_cap;
    memcpy(out, g_hwinfo_buf, n);
    pthread_mutex_unlock(&g_hwinfo_lock);
    if (out_written) *out_written = n;
    return 0;
}

/* ── HW_TEMPS: live sensors with a 1s read cache ────────────────── */

/* Physical sanity bounds. Every sensor value is validated against these
 * before it's reported, regardless of source. Background (FW 5.10 phat,
 * CFI-1115A, hardware-confirmed): the Sony direct sensor APIs return
 * nothing usable on some firmware, and the non-Sony fallbacks we reach
 * for then (sysctl thermal zones, machdep.tsc_freq) DO exist on the
 * underlying FreeBSD but return values that are not real PS5 sensor
 * readings — e.g. dev.cpu.0.temperature came back as ~270202, which the
 * tenths-of-kelvin conversion turned into "26747 °C". An out-of-range
 * value is worse than no value (the UI happily renders "26747 °C"), so
 * anything outside these bounds is dropped and the field reports 0 =
 * "unavailable". A PS5 (incl. Pro, Zen 2 @ up to 3.85 GHz) never
 * legitimately exceeds these. */
#define HW_TEMP_MIN_C        1      /* >0; 0 already means "no reading" */
#define HW_TEMP_MAX_C        150    /* CPUs thermal-throttle ~100 °C */
#define HW_CPU_FREQ_MAX_MHZ  6000   /* generous ceiling over any console CPU */

/* Cache holds ONLY the basic, auto-poll-safe sensors (CPU/SoC temp +
 * CPU clock). The extended telemetry (power, CPU usage, fan duty, product
 * shape) is deliberately NOT cached: it's read fresh on the explicit
 * "Read sensors" request and must never be served to (or populated by) the
 * Dashboard's 5 s basic poll. See hw_temps_get_text_ex. */
typedef struct {
    time_t   last_read;
    int      cpu_temp;
    int      soc_temp;
    int      m2_temp;
    long     cpu_freq_mhz;
    int      valid;
} hw_temps_cache_t;

static pthread_mutex_t g_temps_lock = PTHREAD_MUTEX_INITIALIZER;
static hw_temps_cache_t g_temps_cache = {0};

/* ── Live-sensor read: DIRECT Sony APIs only, on the request thread ──
 *
 * History (all hardware-confirmed) that shaped this:
 *
 *  1. The old ptrace-via-SceShellUI fallback (reached when a direct call
 *     returned 0) HUNG on FW 5.10 (phat, CFI-1115A). Combined with the
 *     desktop's old 5-second sensor auto-poll, every open of the Hardware
 *     tab fired a read that hung the mgmt connection and ultimately
 *     dropped the payload — the reported "the helper disconnects every
 *     time I open Hardware". That fallback is GONE; sensors are read only
 *     via the direct sceKernel* exports below.
 *
 *  2. The non-Sony fallbacks the old code then reached for returned
 *     GARBAGE on PS5: sysctl dev.cpu.0.temperature read ~270202 →
 *     "26747 °C", and the direct cpu-freq read (guarded only by hz>0)
 *     surfaced a ~5.2e13 MHz clock. Every value is now range-checked
 *     against the HW_* physical bounds; anything out of range reports 0 =
 *     "unavailable", which is honest rather than fabricated. The sysctl
 *     thermal-zone temperature fallback is removed entirely.
 *
 *  3. These reads are intentionally INLINE on the mgmt request thread.
 *     An earlier attempt moved them onto a detached helper thread (to add
 *     a watchdog around a possibly-hanging call); on this platform the
 *     Sony sensor APIs SIGSEGV the whole process when invoked from a
 *     freshly-spawned pthread (even with a matched 512 KiB stack), so the
 *     helper thread was strictly worse — it crashed the payload on the
 *     first read of every console. Inline on the mgmt worker (which the
 *     Sony runtime set up) is the only path that reads cleanly. The
 *     companion desktop change makes this safe to keep simple: live
 *     sensors are no longer auto-polled, only read on an explicit
 *     "Read sensors" click, so a read can never fire unattended.
 *
 * 1-second cache so a double-click / rapid re-read coalesces into one
 * round of syscalls. */
/* `extended` selects which sensors are read:
 *   0 = BASIC: CPU temp, SoC temp, CPU clock only. These are the calls
 *       proven safe on every console, and the only ones the Dashboard's
 *       5 s auto-poll is allowed to fire. Cached for 1 s.
 *   1 = EXTENDED: additionally read SoC power, CPU usage, fan duty and
 *       product shape. These are the newer / historically-risky Sony
 *       getters; they run ONLY here, behind the explicit "Read sensors"
 *       click, never on the auto-poll — so a wedge on an untested SKU is
 *       a recoverable, user-triggered event, not a tab-open disconnect.
 *       Read fresh every time (not cached). */
int hw_temps_get_text_ex(int flags, char *out, size_t out_cap,
                         size_t *out_written, const char **err_reason_out) {
    if (!out || out_cap < 256) {
        if (err_reason_out) *err_reason_out = "hw_temps_buffer_too_small";
        return -1;
    }
    hw_resolve_once();

    int cpu_temp = 0, soc_temp = 0, m2_temp = 0;
    long cpu_freq_mhz = 0;
    uint32_t power_mw = 0;
    int cpu_usage_pct = -1, fan_duty_pct = -1, product_shape = -1;

    /* ── Basic sensors (1 s cache; auto-poll-safe) ── */
    pthread_mutex_lock(&g_temps_lock);
    time_t now = time(NULL);
    if (g_temps_cache.valid && (now - g_temps_cache.last_read) < 1) {
        cpu_temp     = g_temps_cache.cpu_temp;
        soc_temp     = g_temps_cache.soc_temp;
        m2_temp      = g_temps_cache.m2_temp;
        cpu_freq_mhz = g_temps_cache.cpu_freq_mhz;
    } else {
        /* All three direct sensor getters are dlsym'd Sony calls — fault-
         * guarded (see hw_guard.h) so a getter that faults under some loader
         * context degrades to "unavailable" instead of dropping the helper.
         * `got` is volatile so it's well-defined on the longjmp path; the
         * out-param is only read when got==1 (so it need not be volatile). */
        if (g_hw.cpu_temp) {
            int t = 0;
            volatile int got = 0;
            HW_GUARD("sceKernelGetCpuTemperature", got = (g_hw.cpu_temp(&t) == 0));
            if (got && t >= HW_TEMP_MIN_C && t <= HW_TEMP_MAX_C) {
                cpu_temp = t;
            }
        }
        /* SoC thermal sensors. Sweep channels 0–7 rather than reading only
         * channel 0: the canonical SoC junction sensor is channel 0 on the
         * phat + Pro (hardware-confirmed), but the channel layout isn't
         * guaranteed across SoC revisions — a Slim or other SKU may surface
         * its usable reading on a different channel. The first in-range
         * channel wins for soc_temp, so on any console where channel 0 is
         * valid (phat/Pro) the result is byte-identical to the old code.
         *
         * Channel 2 is the M.2 NVMe expansion slot sensor (per elf-arsenal's
         * sensors.c mapping). An empty slot returns 0, so require >= 20 to
         * treat as populated. Captured alongside soc_temp in the same sweep
         * — no extra API calls. */
        if (g_hw.soc_temp) {
            HW_GUARD("sceKernelGetSocSensorTemperature", {
                for (int ch = 0; ch < 8; ch++) {
                    int st = 0;
                    if (g_hw.soc_temp(ch, &st) == 0 &&
                        st >= HW_TEMP_MIN_C && st <= HW_TEMP_MAX_C) {
                        if (soc_temp == 0) soc_temp = st;
                        if (ch == 2 && st >= 20) m2_temp = st;
                        if (soc_temp != 0 && m2_temp != 0) break;
                    }
                }
            });
        }
        if (g_hw.cpu_freq) {
            volatile long hz = 0;
            volatile int  got = 0;
            HW_GUARD("sceKernelGetCpuFrequency", { hz = g_hw.cpu_freq(); got = 1; });
            if (got) {
                long mhz = (hz > 0) ? hz / (1000L * 1000L) : 0;
                /* Upper-bound the result: on FW where the direct call returns
                 * a garbage value (FW 5.10 phat returned ~5.2e13 MHz) an
                 * `hz > 0`-only guard would let it straight through. */
                if (mhz > 0 && mhz <= HW_CPU_FREQ_MAX_MHZ) cpu_freq_mhz = mhz;
            }
        }
        /* CPU frequency via kernel TSC — fallback when the direct call
         * gave nothing sane. Pure sysctl, bounded the same way. */
        if (cpu_freq_mhz <= 0) {
            uint64_t tsc = 0;
            if (sysctl_uint64("machdep.tsc_freq", &tsc) == 0 && tsc > 0) {
                long mhz = (long)(tsc / 1000000ULL);
                if (mhz > 0 && mhz <= HW_CPU_FREQ_MAX_MHZ) cpu_freq_mhz = mhz;
            }
        }

        g_temps_cache.cpu_temp     = cpu_temp;
        g_temps_cache.soc_temp     = soc_temp;
        g_temps_cache.m2_temp      = m2_temp;
        g_temps_cache.cpu_freq_mhz = cpu_freq_mhz;
        g_temps_cache.last_read    = now;
        g_temps_cache.valid        = 1;
    }
    pthread_mutex_unlock(&g_temps_lock);

    /* ── Extended sensors (on-demand only, read fresh, NEVER cached) ──
     * Reached only for an explicit "Read sensors" request (flags != 0);
     * the Dashboard auto-poll calls with flags=0 and never trips these.
     * Each getter is gated on its own HW_EXT_* bit so a firmware that
     * misbehaves on ONE can be served the rest by excluding just that bit.
     *
     * The long-standing "FW 9.60 Pro hangs on SoC power" was never the
     * firmware: we were handing the kernel a stack slot ~100 bytes too
     * small and it wrote over the caller's frame. See the ABI note by the
     * typedef. The gate stays because it is useful, not because this call
     * is known-bad. */
    if (flags) {
        /* SoC power draw: sum of the 8 rails, already in milliwatts. No
         * unit guessing — the old "is it W or mW?" heuristic existed only
         * because we were reading a rail's power packed against its voltage
         * and treating the pair as one scalar. */
        soc_power_sample_t soc_pw;
        memset(&soc_pw, 0, sizeof(soc_pw));
        if ((flags & HW_EXT_POWER) && g_hw.soc_power &&
            g_hw.soc_power(&soc_pw) == 0) {
            /* Total SoC draw is the sum of the rails. Each rail's triplet is
             * zero when that rail reports nothing, so summing skips them for
             * free. Range-check per rail so one implausible entry cannot
             * inflate the total. */
            uint64_t total_mw = 0;
            for (int rail = 0; rail < SOC_POWER_RAILS; ++rail) {
                uint32_t rail_mw = soc_pw.u32[rail * SOC_POWER_FIELDS];
                if (rail_mw <= HW_POWER_RAIL_MAX_MW) {
                    total_mw += rail_mw;
                }
            }
            if (total_mw > 0 && total_mw <= HW_POWER_TOTAL_MAX_MW) {
                power_mw = (uint32_t)total_mw;
            }                                   /* else implausible -> 0 */
        }

        /* CPU usage — average across the reported cores (0..100 %). The API
         * fills a per-core array and writes the core count; we pass a
         * 16-int buffer (PS5 has 8 cores; 16 is generous headroom) and
         * average only the cores it says it filled. Each per-core value is
         * range-checked so a stray entry can't skew the mean. */
        if ((flags & HW_EXT_USAGE) && g_hw.cpu_usage) {
            int per_core[16] = {0};
            int core_count = 0;
            if (g_hw.cpu_usage(per_core, &core_count) == 0 &&
                core_count > 0 && core_count <= 16) {
                long sum = 0;
                int counted = 0;
                for (int i = 0; i < core_count; i++) {
                    if (per_core[i] >= 0 && per_core[i] <= 100) {
                        sum += per_core[i];
                        counted++;
                    }
                }
                if (counted > 0) cpu_usage_pct = (int)(sum / counted);
            }
        }

        /* Current fan duty as a percentage. `scratch` is an opaque buffer
         * the API writes into; its required size isn't documented, so we
         * give it a generous zeroed 256-byte buffer (the reference passes a
         * scratch and reads cleanly — 256 is well above any plausible need).
         * The raw duty is a uint16; PS5 fan duty is reported either as a
         * 0..100 percentage or a 0..255 PWM value, so we map >100 down from
         * the 0..255 scale and clamp. Approximate by design (the exact
         * scale is the one thing to confirm on hardware). */
        if ((flags & HW_EXT_FAN) && g_hw.fan_duty) {
            uint16_t duty = 0;
            unsigned char scratch[256] = {0};
            if (g_hw.fan_duty(&duty, scratch) == 0) {
                int pct = (duty <= 100) ? (int)duty
                                        : (int)(((int)duty * 100 + 127) / 255);
                if (pct < 0) pct = 0;
                if (pct > 100) pct = 100;
                fan_duty_pct = pct;
            }
        }

        /* Basic product shape — a raw Sony enum that distinguishes hardware
         * families (standard / slim / Pro / devkit …). Reported raw; the
         * desktop maps the known codes to a label and otherwise shows the
         * number. The model string (sceKernelGetHwModelName, CFI-xxxx) is
         * still the primary identifier; this is a cross-check. */
        if ((flags & HW_EXT_SHAPE) && g_hw.product_shape) {
            int ps = 0;
            if (g_hw.product_shape(&ps) == 0) product_shape = ps;
        }
    }

    int fan_pinned_c = hw_fan_pinned_threshold();

    int n = snprintf(out, out_cap,
        "cpu_temp=%d\n"
        "soc_temp=%d\n"
        "m2_temp=%d\n"
        "cpu_freq_mhz=%ld\n"
        "soc_clock_mhz=0\n"
        "soc_power_mw=%u\n"
        "cpu_usage_pct=%d\n"
        "fan_duty_pct=%d\n"
        "product_shape=%d\n"
        "fan_pinned_c=%d\n",
        cpu_temp, soc_temp, m2_temp, cpu_freq_mhz, power_mw,
        cpu_usage_pct, fan_duty_pct, product_shape, fan_pinned_c);
    if (n < 0 || (size_t)n >= out_cap) {
        if (err_reason_out) *err_reason_out = "hw_temps_format_failed";
        return -1;
    }
    if (out_written) *out_written = (size_t)n;
    return 0;
}

/* Back-compat wrapper: the 4-arg form used by the generic
 * handle_hw_text_op and any other caller reads BASIC sensors only
 * (flags=0), so it can never fire the on-demand-only getters. */
int hw_temps_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out) {
    return hw_temps_get_text_ex(0, out, out_cap, out_written, err_reason_out);
}

/* ── HW_POWER: uptime via kern.boottime (no Sony APIs) ──────────── */

int hw_power_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out) {
    if (!out || out_cap < 128) {
        if (err_reason_out) *err_reason_out = "hw_power_buffer_too_small";
        return -1;
    }
    struct timeval boottime;
    size_t bt_len = sizeof(boottime);
    uint64_t uptime_sec = 0;
    if (sysctlbyname("kern.boottime", &boottime, &bt_len, NULL, 0) == 0 &&
        boottime.tv_sec > 0) {
        struct timeval nowtv;
        gettimeofday(&nowtv, NULL);
        if (nowtv.tv_sec > boottime.tv_sec) {
            uptime_sec = (uint64_t)(nowtv.tv_sec - boottime.tv_sec);
        }
    }
    uint64_t hours   = uptime_sec / 3600;
    uint64_t minutes = (uptime_sec % 3600) / 60;

    /* System load average (1, 5, 15 min) via getloadavg — added to
     * the SDK's libc in the 2026-05 ps5-payload-dev/sdk update. Same
     * shape as BSD/Linux: 0.00 = idle, 1.00 per logical core = fully
     * loaded. Returns -1 on failure; we report `-1.00` so the
     * desktop can distinguish "unavailable" from "0.00 idle".
     *
     * We scale to centi-units (× 100) in the wire format because the
     * key=value parser on the engine side is integer-only — the
     * existing field family (operating_time_*, boot_count) is all
     * %llu. Engine divides by 100 for display. */
    double la[3] = { -1.0, -1.0, -1.0 };
    int la_count = getloadavg(la, 3);
    long la_1m  = la_count > 0 ? (long)(la[0] * 100.0) : -100;
    long la_5m  = la_count > 1 ? (long)(la[1] * 100.0) : -100;
    long la_15m = la_count > 2 ? (long)(la[2] * 100.0) : -100;

    int n = snprintf(out, out_cap,
        "operating_time_sec=%llu\n"
        "operating_time_hours=%llu\n"
        "operating_time_minutes=%llu\n"
        "boot_count=0\n"
        "power_consumption_mw=0\n"
        "load_avg_1m_centi=%ld\n"
        "load_avg_5m_centi=%ld\n"
        "load_avg_15m_centi=%ld\n",
        (unsigned long long)uptime_sec,
        (unsigned long long)hours,
        (unsigned long long)minutes,
        la_1m, la_5m, la_15m);
    if (n < 0 || (size_t)n >= out_cap) {
        if (err_reason_out) *err_reason_out = "hw_power_format_failed";
        return -1;
    }
    if (out_written) *out_written = (size_t)n;
    return 0;
}

/* ── HW_STORAGE: "Console Storage" aggregate ─────────────────────
 *
 * Approximates PS5 Settings → Storage → Console Storage:
 *   Total = /user total + /system_data total + /system_ex total
 *   Free  = /user bavail + /system_data bavail + /system_ex bavail
 *   Used  = Total - Free
 *
 * `bavail` is what the kernel reports as "available to non-root"
 * users; `bfree` is the raw free count. The difference is the
 * "reserved" pool the filesystem keeps so root can still write
 * even when bavail hits zero.
 *
 * Reserve model — confirmed against a live PS5 (FS_LIST_VOLUMES +
 * HW_STORAGE on a 2 TB Pro): Settings COUNTS THE UFS RESERVE AS USED,
 * it does not shave it off the headline total. For that console the
 * raw /user partition is f_blocks×f_bsize ≈ 1.96 TB with a ~292 GB
 * (15%) reserve; Settings shows Size ≈ 1.89 TB / Used ≈ 1.72 TB. So
 * total must be the FULL partition (not total-reserved) and `used` =
 * total-bavail folds the reserve into used, matching Settings to a
 * few GB. An earlier build subtracted reserved from total; that made
 * the headline ~292 GB too small and made `used` disagree with
 * Settings by the whole reserve, which is what this corrects. A small
 * residual (~70 GB) remains because Settings also hides a fixed system
 * reserve that statfs can't see — that gap needs a Sony content-
 * manager API to close and is out of scope here.
 *
 * Each statfs failure is non-fatal — the missing partition just
 * contributes zero. /system_ex may not be mounted on some firmware
 * variants and that's fine; the user still gets a /user-only
 * total which is the bulk of the storage anyway. */
#include <sys/mount.h>

static void storage_read_part(const char *path,
                               uint64_t *total_out, uint64_t *bfree_out,
                               uint64_t *bavail_out) {
    *total_out = *bfree_out = *bavail_out = 0;
    struct statfs sf;
    if (statfs(path, &sf) != 0) return;
    uint64_t bs = (uint64_t)sf.f_bsize;
    *total_out  = (uint64_t)sf.f_blocks * bs;
    *bfree_out  = (uint64_t)sf.f_bfree  * bs;
    /* f_bavail can underflow on a near-full filesystem (FreeBSD
     * reports bavail as a signed 64; bfree - bavail = reserved).
     * Treat any value <= 0 as "use bfree" so we don't surface a
     * negative free count. */
    *bavail_out = (sf.f_bavail > 0) ? (uint64_t)sf.f_bavail * bs : *bfree_out;
}

int hw_storage_get_text(char *out, size_t out_cap, size_t *out_written,
                         const char **err_reason_out) {
    if (!out || out_cap < 256) {
        if (err_reason_out) *err_reason_out = "hw_storage_buffer_too_small";
        return -1;
    }

    uint64_t u_total = 0, u_bfree = 0, u_bavail = 0;
    storage_read_part("/user", &u_total, &u_bfree, &u_bavail);
    /* Reserved = bfree - bavail (the slice the FS keeps for root). Kept
     * for the reserved_bytes telemetry fields below; it is NOT shaved
     * off total — Settings counts it as used, so we let used_bytes =
     * total - bavail absorb it (see the header comment). */
    uint64_t u_reserved = (u_bfree > u_bavail) ? (u_bfree - u_bavail) : 0;

    uint64_t sd_total = 0, sd_bfree = 0, sd_bavail = 0;
    storage_read_part("/system_data", &sd_total, &sd_bfree, &sd_bavail);

    uint64_t sx_total = 0, sx_bfree = 0, sx_bavail = 0;
    storage_read_part("/system_ex", &sx_total, &sx_bfree, &sx_bavail);

    uint64_t total_bytes = u_total + sd_total + sx_total;
    uint64_t free_bytes  = u_bavail + sd_bavail + sx_bavail;
    uint64_t used_bytes  = (total_bytes > free_bytes) ? (total_bytes - free_bytes) : 0;

    int n = snprintf(out, out_cap,
        "total_bytes=%llu\n"
        "free_bytes=%llu\n"
        "used_bytes=%llu\n"
        "reserved_bytes=%llu\n"
        "user_total_bytes=%llu\n"
        "user_free_bytes=%llu\n"
        "user_reserved_bytes=%llu\n"
        "system_data_total_bytes=%llu\n"
        "system_data_free_bytes=%llu\n"
        "system_ex_total_bytes=%llu\n"
        "system_ex_free_bytes=%llu\n",
        (unsigned long long)total_bytes,
        (unsigned long long)free_bytes,
        (unsigned long long)used_bytes,
        (unsigned long long)u_reserved,
        (unsigned long long)u_total,
        (unsigned long long)u_bavail,
        (unsigned long long)u_reserved,
        (unsigned long long)sd_total,
        (unsigned long long)sd_bavail,
        (unsigned long long)sx_total,
        (unsigned long long)sx_bavail);
    if (n < 0 || (size_t)n >= out_cap) {
        if (err_reason_out) *err_reason_out = "hw_storage_format_failed";
        return -1;
    }
    if (out_written) *out_written = (size_t)n;
    return 0;
}

/* ── Fan auto-reapply watcher ─────────────────────────────────────
 *
 * State lives in two atomics:
 *   g_pinned_threshold_c — the value to keep re-applying. 0 means
 *     "nothing pinned"; the watcher early-exits on a 0 tick so a
 *     payload that never sets fan stays at zero cost.
 *   g_fan_watcher_started — guard for one-shot lazy launch. We use
 *     atomic_exchange so the second concurrent caller sees the prior
 *     "1" return value and bails before pthread_create runs again.
 *
 * Why lazy-start instead of starting from runtime_init: most ps5upload
 * sessions never touch the fan (the user only sent files), so paying
 * for an idle pthread + 15s wake-up tick on every payload boot would
 * be wasted work. First successful fan set wakes the watcher; once
 * armed it stays for the payload's lifetime (detached, no join). */
static atomic_int g_pinned_threshold_c   = 0;
static atomic_int g_fan_watcher_started  = 0;
/* Reapply interval in seconds (user-configurable, persisted). Loaded
 * from fan_reapply.conf at boot, defaults to FAN_REAPPLY_DEFAULT_SEC.
 * The watcher thread reads this every cycle so a runtime change takes
 * effect on the next tick without restarting the thread. */
static atomic_int g_fan_reapply_sec = FAN_REAPPLY_DEFAULT_SEC;
/* Serializes the (ioctl, pin) sequence inside `hw_fan_set_threshold`.
 *
 * Two concurrent FTX2 callers setting different thresholds could
 * otherwise interleave: A opens/ioctls 50 → kernel state = 50;
 * B opens/ioctls 70 → kernel state = 70; A pins 50 → atomic = 50;
 * B pins 70 → atomic = 70.   That sequence ends consistent, but
 * SWAP one pair: A's ioctl 50 → kernel 50, B's ioctl 70 → kernel 70,
 * B's pin 70 → atomic 70, A's pin 50 → atomic 50.   Now the watcher
 * drives kernel back to 50 every 15s even though B was the most-
 * recent caller. Holding this mutex across the whole sequence
 * forces last-writer-wins consistency. */
static pthread_mutex_t g_fan_set_mtx = PTHREAD_MUTEX_INITIALIZER;

int hw_fan_pinned_threshold(void) {
    return atomic_load(&g_pinned_threshold_c);
}

void hw_fan_pin_threshold(uint8_t threshold_c) {
    if (threshold_c < HW_FAN_THRESHOLD_MIN) threshold_c = HW_FAN_THRESHOLD_MIN;
    if (threshold_c > HW_FAN_THRESHOLD_MAX) threshold_c = HW_FAN_THRESHOLD_MAX;
    atomic_store(&g_pinned_threshold_c, (int)threshold_c);
}

/* ── Persistence ──────────────────────────────────────────────────
 *
 * The pinned threshold is saved to a one-line file at
 * /data/ps5upload/fan_threshold.conf so it survives payload redeploy
 * and console reboot. Loaded once at boot from `hw_fan_load_persisted`
 * (called by main after runtime_ensure_directories). Saved on every
 * successful set inside `hw_fan_set_threshold`.
 *
 * File format is a single decimal integer (the °C threshold). We
 * intentionally use a trivial format — no JSON, no key=value — so
 * the file is easy to inspect/edit via FTP and can't be corrupted
 * by a half-written JSON parser. */

#define FAN_PERSIST_PATH PS5UPLOAD2_RUNTIME_ROOT "/fan_threshold.conf"
#define FAN_REAPPLY_PERSIST_PATH PS5UPLOAD2_RUNTIME_ROOT "/fan_reapply.conf"

/* Returns the persisted threshold, or 0 if no valid file exists.
 * The caller (main) treats 0 as "nothing to restore". */
int hw_fan_load_persisted(void) {
    FILE *fp = fopen(FAN_PERSIST_PATH, "r");
    if (!fp) return 0;

    int val = 0;
    int matched = fscanf(fp, "%d", &val);
    fclose(fp);

    if (matched != 1 || val < HW_FAN_THRESHOLD_MIN || val > HW_FAN_THRESHOLD_MAX) {
        /* Stale/corrupt file — treat as unset. Don't delete it here
         * (avoids a surprising side-effect from a read-only API);
         * the next successful set will overwrite it cleanly. */
        return 0;
    }
    return val;
}

/* Saves the threshold to the persist file. Best-effort: a write
 * failure (e.g., /data/ps5upload doesn't exist yet on a brand-new
 * console) is silently ignored — the in-memory pin still works for
 * this session, and the next successful set will retry the write. */
static void hw_fan_save_persisted(uint8_t threshold_c) {
    FILE *fp = fopen(FAN_PERSIST_PATH, "w");
    if (!fp) return;
    fprintf(fp, "%u\n", (unsigned)threshold_c);
    fclose(fp);
}

/* ── Reapply interval persistence ──────────────────────────────────
 *
 * Same trivial one-line-file format as the threshold. Loaded once at
 * boot from hw_fan_load_reapply_interval (called by main alongside
 * hw_fan_load_persisted). Saved on every successful call to
 * hw_fan_set_reapply_interval. */

int hw_fan_load_reapply_interval(void) {
    FILE *fp = fopen(FAN_REAPPLY_PERSIST_PATH, "r");
    if (!fp) return FAN_REAPPLY_DEFAULT_SEC;
    int val = 0;
    int matched = fscanf(fp, "%d", &val);
    fclose(fp);
    if (matched != 1 || val < FAN_REAPPLY_MIN_SEC || val > FAN_REAPPLY_MAX_SEC)
        return FAN_REAPPLY_DEFAULT_SEC;
    return val;
}

static void hw_fan_save_reapply_interval(int seconds) {
    FILE *fp = fopen(FAN_REAPPLY_PERSIST_PATH, "w");
    if (!fp) return;
    fprintf(fp, "%d\n", seconds);
    fclose(fp);
}

int hw_fan_reapply_interval(void) {
    return atomic_load(&g_fan_reapply_sec);
}

void hw_fan_set_reapply_interval(int seconds) {
    if (seconds < FAN_REAPPLY_MIN_SEC) seconds = FAN_REAPPLY_MIN_SEC;
    if (seconds > FAN_REAPPLY_MAX_SEC) seconds = FAN_REAPPLY_MAX_SEC;
    atomic_store(&g_fan_reapply_sec, seconds);
    hw_fan_save_reapply_interval(seconds);
}

/* Forward decl — defined below the setter so it can share the same
 * fd open/ioctl pattern via a static helper. */
static int hw_fan_apply_locked(uint8_t threshold_c);

static void *hw_fan_watcher_thread_fn(void *arg) {
    (void)arg;
    /* Best-effort thread name for ps/top output; ignored if the
     * syscall isn't available. SYS_thr_set_name is FreeBSD-specific
     * (matches sonicloader fan.c:170). */
    (void)syscall(SYS_thr_set_name, -1, "ps5upload-fan");

    for (;;) {
        /* Sleep in 1 s chunks rather than one long sleep so a future
         * shutdown signal could break out cheaply if we ever add one.
         * Read the interval each iteration so a runtime change via
         * hw_fan_set_reapply_interval() takes effect on the next cycle. */
        int interval = atomic_load(&g_fan_reapply_sec);
        if (interval < FAN_REAPPLY_MIN_SEC) interval = FAN_REAPPLY_MIN_SEC;
        if (interval > FAN_REAPPLY_MAX_SEC) interval = FAN_REAPPLY_MAX_SEC;
        for (int i = 0; i < interval; i++) sleep(1);

        int t = atomic_load(&g_pinned_threshold_c);
        if (t < HW_FAN_THRESHOLD_MIN || t > HW_FAN_THRESHOLD_MAX) {
            /* No pin set, or pin was clobbered to a sentinel value;
             * just spin idle. Cheaper than tearing the thread down +
             * re-launching it on the next pin. */
            continue;
        }

        /* Re-apply failures are non-fatal — the firmware may be in a
         * brief state (e.g., suspend transition) where /dev/icc_fan is
         * busy. Next tick will retry. We deliberately don't log here
         * to avoid spamming the klog during normal launch transitions
         * (the device is briefly unavailable mid-transition). */
        (void)hw_fan_apply_locked((uint8_t)t);
    }
    return NULL;
}

/* Lazy idempotent start. atomic_exchange returns the prior value, so
 * the first caller sees 0 (and launches), every subsequent caller
 * sees 1 (and returns without touching pthread). */
static void hw_fan_watcher_start_once(void) {
    if (atomic_exchange(&g_fan_watcher_started, 1)) return;

    pthread_t thread;
    pthread_attr_t attr;
    if (pthread_attr_init(&attr) != 0) {
        /* Roll the "started" flag back so a future call can retry. */
        atomic_store(&g_fan_watcher_started, 0);
        return;
    }
    /* Detached — we never join. Avoids leaking a joinable thread
     * handle on shutdown paths that don't pthread_join. */
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&thread, &attr, hw_fan_watcher_thread_fn, NULL) != 0) {
        /* Same rollback as the attr_init failure above so we can retry. */
        atomic_store(&g_fan_watcher_started, 0);
    }
    pthread_attr_destroy(&attr);
}

/* Bare ioctl call — no logging, no pin update, no watcher start. The
 * watcher thread uses this so its ticks don't recursively re-arm
 * themselves or print to stdout. */
static int hw_fan_apply_locked(uint8_t threshold_c) {
    int fd = open(ICC_FAN_DEVICE_NODE, O_RDONLY);
    if (fd < 0) return -1;

    unsigned char cmd[ICC_FAN_CMD_LEN] = {0};
    cmd[ICC_FAN_THRESHOLD_OFFSET] = threshold_c;
    int rc = ioctl(fd, ICC_FAN_IOCTL_SET_THRESHOLD, cmd);
    close(fd);
    return rc;
}

int hw_fan_set_threshold(uint8_t threshold_c, const char **err_reason_out) {
    /* Clamp. Intentionally silent — the client UI also clamps, but
     * we enforce here so a malicious/buggy caller can't bypass it by
     * talking FTX2 directly. Out-of-range values get pulled to the
     * nearest safe bound rather than rejected, so the user still gets
     * a working outcome. */
    if (threshold_c < HW_FAN_THRESHOLD_MIN) threshold_c = HW_FAN_THRESHOLD_MIN;
    if (threshold_c > HW_FAN_THRESHOLD_MAX) threshold_c = HW_FAN_THRESHOLD_MAX;

    /* Serialize so two concurrent callers can't end up with the
     * kernel-state and the pin-atomic carrying different values
     * (which would cause the watcher to drive the kernel back to
     * whichever caller pinned last regardless of which ioctl
     * landed last). See g_fan_set_mtx comment for the race. */
    pthread_mutex_lock(&g_fan_set_mtx);

    int fd = open(ICC_FAN_DEVICE_NODE, O_RDONLY);
    if (fd < 0) {
        pthread_mutex_unlock(&g_fan_set_mtx);
        if (err_reason_out) *err_reason_out = "icc_fan_open_failed";
        return -1;
    }

    unsigned char cmd[ICC_FAN_CMD_LEN] = {0};
    cmd[ICC_FAN_THRESHOLD_OFFSET] = threshold_c;

    int rc = ioctl(fd, ICC_FAN_IOCTL_SET_THRESHOLD, cmd);
    int saved_errno = errno;
    close(fd);

    if (rc < 0) {
        pthread_mutex_unlock(&g_fan_set_mtx);
        /* On firmwares where the ioctl is refused we keep a generic
         * reason — surfacing errno to the client would leak FreeBSD-
         * specific codes that aren't actionable. Useful local debug
         * info stays in saved_errno for a future logging hook. */
        (void)saved_errno;
        if (err_reason_out) *err_reason_out = "icc_fan_ioctl_failed";
        return -1;
    }

    /* On success, pin the value and arm the auto-reapply watcher.
     * Order matters: pin first, then start. If we started first and
     * the thread happened to tick before atomic_store landed, it
     * would early-exit on the still-zero pin and skip a cycle.
     *
     * Persist to disk after pinning so a payload redeploy or console
     * reboot restores the same threshold automatically (matches
     * elf-arsenal's config_save() call after a successful fan set). */
    hw_fan_pin_threshold(threshold_c);
    hw_fan_save_persisted(threshold_c);
    pthread_mutex_unlock(&g_fan_set_mtx);
    hw_fan_watcher_start_once();
    return 0;
}
