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

#include <ps5/kernel.h>

#include "hw_info.h"
#include "shellui_rpc.h"

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
 * Going below 5 s would just burn extra ioctls for no thermal gain. */
#define FAN_REAPPLY_SEC 15

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
/* Correct ABI is (uint64_t *out, double reserved) — confirmed against a
 * working reference impl (Elf Arsenal) that reads this successfully. Our
 * earlier `(uint32_t *)` declaration was wrong on two counts: the out
 * pointer is 64-bit (a uint32 target takes an 8-byte write = a 4-byte
 * stack overflow), and the trailing double arg was missing. That UB is a
 * strong candidate for the "hang/disconnect" we previously blamed on the
 * API. Signature fixed; the call stays gated below (see hw_temps_get_text)
 * pending a hardware retest. */
typedef int     (*sceKernelGetSocPowerConsumption_fn)(uint64_t *out, double reserved);
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

        if (!g_hw.model_name || g_hw.model_name(model_name) != 0 || model_name[0] == '\0') {
            snprintf(model_name, sizeof(model_name), "%s", "PlayStation 5");
        }
        if (!g_hw.serial || g_hw.serial(serial) != 0 || serial[0] == '\0') {
            snprintf(serial, sizeof(serial), "%s", "N/A");
        }
        sysctl_string("hw.machine",     hw_machine, sizeof(hw_machine));
        sysctl_string("kern.ostype",    ostype,     sizeof(ostype));
        sysctl_string("kern.osrelease", osrelease,  sizeof(osrelease));
        sysctl_int("hw.ncpu", &ncpu);

        /* Physical RAM. 5-step detection chain: PS5-specific API
         * first (most accurate), then sysctl variants, then page
         * math, then the 16 GiB default every PS5 is known to have. */
        uint64_t physmem = 0;
        if (g_hw.direct_mem) physmem = (uint64_t)g_hw.direct_mem();
        if (physmem == 0) sysctl_uint64("hw.physmem", &physmem);
        if (physmem == 0) sysctl_uint64("hw.realmem", &physmem);
        if (physmem == 0) sysctl_uint64("hw.usermem", &physmem);
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

        /* Precise firmware version WAS read here via the SDK's kernel-R/W
         * helper (kernel_get_fw_version → KERNEL_ADDRESS_DATA_BASE + a
         * FW-specific offset). Disabled on the always-on Hardware-tab path:
         * it's the only kernel-memory read in that path, and a kernel read
         * at a wrong/unmapped offset faults hard and kills the helper —
         * unlike a Sony-API miss, which just returns an error.
         *
         * Symptom: on PS5 Slim (CFI-2000) the Hardware tab dropped the
         * payload the instant it loaded. The disconnect fix was validated
         * on Pro 9.60 + phat 5.10, but the Slim is a different SKU/FW the
         * SDK's data-base offset isn't proven against; the speculative
         * kernel read is not worth crashing the whole tab for.
         *
         * Cost of disabling: none functionally — the desktop already
         * derives the firmware family from the kern.version string when
         * this word is 0 (its documented fallback). We report 0 here. If
         * Settings-grade precision is ever wanted back, do it behind an
         * explicit, user-initiated read (like the on-demand sensor button),
         * never on the auto-poll. */
        uint32_t kfw = 0;

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

    int cpu_temp = 0, soc_temp = 0;
    long cpu_freq_mhz = 0;
    uint32_t power_mw = 0;
    int cpu_usage_pct = -1, fan_duty_pct = -1, product_shape = -1;

    /* ── Basic sensors (1 s cache; auto-poll-safe) ── */
    pthread_mutex_lock(&g_temps_lock);
    time_t now = time(NULL);
    if (g_temps_cache.valid && (now - g_temps_cache.last_read) < 1) {
        cpu_temp     = g_temps_cache.cpu_temp;
        soc_temp     = g_temps_cache.soc_temp;
        cpu_freq_mhz = g_temps_cache.cpu_freq_mhz;
    } else {
        int t = 0;
        if (g_hw.cpu_temp && g_hw.cpu_temp(&t) == 0 &&
            t >= HW_TEMP_MIN_C && t <= HW_TEMP_MAX_C) {
            cpu_temp = t;
        }
        /* SoC thermal sensor. Sweep channels 0–7 rather than reading only
         * channel 0: the canonical SoC junction sensor is channel 0 on the
         * phat + Pro (hardware-confirmed), but the channel layout isn't
         * guaranteed across SoC revisions — a Slim or other SKU may surface
         * its usable reading on a different channel, in which case a
         * channel-0-only read would report "unavailable" while a real value
         * sits one channel over. Matches the 0–7 sweep the Elf Arsenal
         * reference uses. The first in-range channel wins, so on any console
         * where channel 0 is valid (phat/Pro) the result is byte-identical
         * to the old code — zero regression — and other SKUs gain coverage.
         * Same direct API we already call cleanly for channel 0; only the
         * channel arg varies, and each value is bounds-checked. */
        if (g_hw.soc_temp) {
            for (int ch = 0; ch < 8; ch++) {
                int st = 0;
                if (g_hw.soc_temp(ch, &st) == 0 &&
                    st >= HW_TEMP_MIN_C && st <= HW_TEMP_MAX_C) {
                    soc_temp = st;
                    break;
                }
            }
        }
        if (g_hw.cpu_freq) {
            long hz = g_hw.cpu_freq();
            long mhz = (hz > 0) ? hz / (1000L * 1000L) : 0;
            /* Upper-bound the result: on FW where the direct call returns
             * a garbage value (FW 5.10 phat returned ~5.2e13 MHz) an
             * `hz > 0`-only guard would let it straight through. */
            if (mhz > 0 && mhz <= HW_CPU_FREQ_MAX_MHZ) cpu_freq_mhz = mhz;
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
        g_temps_cache.cpu_freq_mhz = cpu_freq_mhz;
        g_temps_cache.last_read    = now;
        g_temps_cache.valid        = 1;
    }
    pthread_mutex_unlock(&g_temps_lock);

    /* ── Extended sensors (on-demand only, read fresh, NEVER cached) ──
     * Reached only for an explicit "Read sensors" request (flags != 0);
     * the Dashboard auto-poll calls with flags=0 and never trips these.
     * Each getter is gated on its own HW_EXT_* bit so a firmware that
     * wedges on ONE (FW 9.60 Pro hangs on SoC power, HW-confirmed) can be
     * served the rest by excluding just that bit. */
    if (flags) {
        /* SoC power draw. Re-enabled with the CORRECTED ABI
         *   int sceKernelGetSocPowerConsumption(uint64_t *out, double reserved)
         * (was wrongly declared (uint32_t *) — a 32-bit out pointer let the
         * kernel write 8 bytes into a 4-byte stack slot = corruption, and
         * the missing `double` arg left a garbage register; that UB is the
         * likely real cause of the "hang/disconnect" we'd blamed on the API
         * on FW 9.60 Pro).
         *
         * Units: the reference impl labels the result a "W guess", so we
         * normalise defensively. A plausible value <=1000 is taken as
         * watts → mW; a larger one (<=1e6) is taken as already-mW; anything
         * else is rejected. The engine + UI re-validate against the
         * SENSOR_POWER_MAX_MW (=500 W) ceiling, so a wrong guess can only
         * under-report, never render garbage. */
        uint64_t soc_pw = 0;
        if ((flags & HW_EXT_POWER) && g_hw.soc_power &&
            g_hw.soc_power(&soc_pw, 0.0) == 0 && soc_pw > 0) {
            if (soc_pw <= 1000ULL) {
                power_mw = (uint32_t)(soc_pw * 1000ULL);     /* watts → mW */
            } else if (soc_pw <= 1000000ULL) {
                power_mw = (uint32_t)soc_pw;                 /* already mW */
            }                                                /* else implausible → 0 */
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

    int n = snprintf(out, out_cap,
        "cpu_temp=%d\n"
        "soc_temp=%d\n"
        "cpu_freq_mhz=%ld\n"
        "soc_clock_mhz=0\n"
        "soc_power_mw=%u\n"
        "cpu_usage_pct=%d\n"
        "fan_duty_pct=%d\n"
        "product_shape=%d\n",
        cpu_temp, soc_temp, cpu_freq_mhz, power_mw,
        cpu_usage_pct, fan_duty_pct, product_shape);
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
        /* Sleep in 1 s chunks rather than one 15 s sleep so a future
         * shutdown signal could break out cheaply if we ever add one. */
        for (int i = 0; i < FAN_REAPPLY_SEC; i++) sleep(1);

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
     * would early-exit on the still-zero pin and skip a cycle. */
    hw_fan_pin_threshold(threshold_c);
    pthread_mutex_unlock(&g_fan_set_mtx);
    hw_fan_watcher_start_once();
    return 0;
}
