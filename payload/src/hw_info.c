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
#include <time.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/time.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <dlfcn.h>

#include "hw_info.h"
#include "shellui_rpc.h"

/* ── Fan control (/dev/icc_fan) ──────────────────────────────────
 *
 * The /dev/icc_fan ioctl 0xC01C8F07 is the canonical fan-threshold
 * setter used across the PS5 homebrew ecosystem. The magic code
 * encodes:
 *   0xC01C8F07 = _IOC(IOC_INOUT, 'Å', 0x07, 28)   // 'Å' (0x8F) = ICC group
 * and the 10-byte buffer carries the threshold at offset 5.
 *
 * Why the O_RDONLY open flag with a WRITE ioctl: FreeBSD's ioctl
 * permission check allows RDONLY fds for IOC_INOUT commands. Both
 * reference implementations open RDONLY — keeping the same flag
 * here means if Sony ever tightens the check (e.g., requires RDWR
 * on future firmware), our behavior will fail the same way theirs
 * does, and we can match their eventual fix. */
#define ICC_FAN_IOCTL_SET_THRESHOLD 0xC01C8F07UL
#define ICC_FAN_DEVICE_NODE         "/dev/icc_fan"
#define ICC_FAN_CMD_LEN             10
#define ICC_FAN_THRESHOLD_OFFSET    5

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
typedef int     (*sceKernelGetSocPowerConsumption_fn)(uint32_t *power_mw);

typedef struct {
    int   resolved;
    sceKernelGetHwModelName_fn           model_name;
    sceKernelGetHwSerialNumber_fn        serial;
    sceKernelGetCpuTemperature_fn        cpu_temp;
    sceKernelGetSocSensorTemperature_fn  soc_temp;
    sceKernelGetCpuFrequency_fn          cpu_freq;
    sceKernelGetDirectMemorySize_fn      direct_mem;
    sceKernelGetSocPowerConsumption_fn   soc_power;
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
            strcpy(model_name, "PlayStation 5");
        }
        if (!g_hw.serial || g_hw.serial(serial) != 0 || serial[0] == '\0') {
            strcpy(serial, "N/A");
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

        int n = snprintf(g_hwinfo_buf, sizeof(g_hwinfo_buf),
            "model=%s\n"
            "serial=%s\n"
            "has_wlan_bt=1\n"
            "has_optical_out=0\n"
            "hw_model=%s\n"
            "hw_machine=%s\n"
            "os=%s %s\n"
            "ncpu=%d\n"
            "physmem=%llu\n",
            model_name, serial, model_name, hw_machine,
            ostype, osrelease, ncpu, (unsigned long long)physmem);
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

/* ── HW_TEMPS: live sensors with 1s cache + 5s throttle on power ── */

typedef struct {
    time_t   last_read;
    int      cpu_temp;
    int      soc_temp;
    long     cpu_freq_mhz;
    uint32_t power_mw;
    int      valid;
} hw_temps_cache_t;

static pthread_mutex_t g_temps_lock = PTHREAD_MUTEX_INITIALIZER;
static hw_temps_cache_t g_temps_cache = {0};
/* Power API is measurably heavier than the others; throttle further. */
static time_t   g_last_power_read = 0;
static uint32_t g_last_power_mw   = 0;

int hw_temps_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out) {
    if (!out || out_cap < 256) {
        if (err_reason_out) *err_reason_out = "hw_temps_buffer_too_small";
        return -1;
    }
    hw_resolve_once();

    int cpu_temp = 0, soc_temp = 0;
    long cpu_freq_mhz = 0;
    uint32_t power_mw = 0;

    pthread_mutex_lock(&g_temps_lock);
    time_t now = time(NULL);
    if (g_temps_cache.valid && (now - g_temps_cache.last_read) < 1) {
        cpu_temp     = g_temps_cache.cpu_temp;
        soc_temp     = g_temps_cache.soc_temp;
        cpu_freq_mhz = g_temps_cache.cpu_freq_mhz;
        power_mw     = g_temps_cache.power_mw;
    } else {
        /* Sony sensor APIs are now routed through the SceShellUI
         * RPC layer — see payload/src/shellui_rpc.c. The direct
         * calls (g_hw.cpu_temp etc.) wedge from a userland payload
         * on FW 9.60 because Sony's stubs check the caller pid
         * against SceShellUI; the RPC layer attaches via ptrace,
         * runs the same call on ShellUI's stack, returns the
         * result. ShellUI's pid IS what Sony wants, so the call
         * returns normally.
         *
         * Init is idempotent — the first call latches the resolved
         * addresses, subsequent calls become no-ops. The
         * field-marked addresses on a firmware that doesn't export
         * a particular symbol stay 0; the RPC returns -1 and the
         * field reads as 0 in the UI. */
        (void)shellui_rpc_init();
        if (shellui_rpc_ready()) {
            int t = 0;
            if (shellui_rpc_get_cpu_temp(&t) == 0 && t > 0 && t < 200) {
                cpu_temp = t;
            }
            t = 0;
            if (shellui_rpc_get_soc_temp(&t) == 0 && t > 0 && t < 200) {
                soc_temp = t;
            }
            long hz = 0;
            if (shellui_rpc_get_cpu_freq_hz(&hz) == 0 && hz > 0) {
                cpu_freq_mhz = hz / (1000L * 1000L);
            }
        }
        /* Suppress unused-symbol warnings — these dlsym pointers
         * remain resolved as a fallback for firmwares where direct
         * calls would have worked. */
        (void)g_hw.cpu_temp;
        (void)g_hw.soc_temp;
        (void)g_hw.cpu_freq;
        (void)g_hw.soc_power;

        /* CPU frequency via kernel TSC — fallback when neither RPC
         * nor direct call returned. */
        if (cpu_freq_mhz <= 0) {
            uint64_t tsc = 0;
            if (sysctl_uint64("machdep.tsc_freq", &tsc) == 0 && tsc > 0) {
                cpu_freq_mhz = (long)(tsc / 1000000ULL);
            }
        }

        /* SoC power — routed through the same RPC. 5-second
         * file-scope throttle on top of the 1-second sensor cache
         * because each RPC call is a full ptrace attach/detach
         * round-trip (~tens of ms) which is heavier than the temp
         * reads. */
        if (shellui_rpc_ready() && (now - g_last_power_read) >= 5) {
            uint32_t pw = 0;
            if (shellui_rpc_get_soc_power_mw(&pw) == 0 && pw < 500000u) {
                g_last_power_mw = pw;
            }
            g_last_power_read = now;
        }
        power_mw = g_last_power_mw;
        (void)g_hw.soc_power;

        /* Sysctl-based temperature fallbacks. None of these are
         * documented to work on PS5 specifically; if any returns
         * non-zero we use it. Values on FreeBSD are tenths of a
         * kelvin; we convert back to celsius. */
        if (cpu_temp == 0) {
            int tz = 0;
            if (sysctl_int("dev.cpu.0.temperature", &tz) == 0 && tz > 0) {
                cpu_temp = (tz - 2732) / 10;
            } else if (sysctl_int("hw.acpi.thermal.tz0.temperature", &tz) == 0 && tz > 0) {
                cpu_temp = (tz - 2732) / 10;
            }
        }
        if (soc_temp == 0) {
            int tz = 0;
            if (sysctl_int("dev.cpu.1.temperature", &tz) == 0 && tz > 0) {
                soc_temp = (tz - 2732) / 10;
            } else if (sysctl_int("hw.acpi.thermal.tz1.temperature", &tz) == 0 && tz > 0) {
                soc_temp = (tz - 2732) / 10;
            }
        }

        g_temps_cache.last_read    = now;
        g_temps_cache.cpu_temp     = cpu_temp;
        g_temps_cache.soc_temp     = soc_temp;
        g_temps_cache.cpu_freq_mhz = cpu_freq_mhz;
        g_temps_cache.power_mw     = power_mw;
        g_temps_cache.valid        = 1;
    }
    pthread_mutex_unlock(&g_temps_lock);

    int n = snprintf(out, out_cap,
        "cpu_temp=%d\n"
        "soc_temp=%d\n"
        "cpu_freq_mhz=%ld\n"
        "soc_clock_mhz=0\n"
        "soc_power_mw=%u\n",
        cpu_temp, soc_temp, cpu_freq_mhz, power_mw);
    if (n < 0 || (size_t)n >= out_cap) {
        if (err_reason_out) *err_reason_out = "hw_temps_format_failed";
        return -1;
    }
    if (out_written) *out_written = (size_t)n;
    return 0;
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

    int n = snprintf(out, out_cap,
        "operating_time_sec=%llu\n"
        "operating_time_hours=%llu\n"
        "operating_time_minutes=%llu\n"
        "boot_count=0\n"
        "power_consumption_mw=0\n",
        (unsigned long long)uptime_sec,
        (unsigned long long)hours,
        (unsigned long long)minutes);
    if (n < 0 || (size_t)n >= out_cap) {
        if (err_reason_out) *err_reason_out = "hw_power_format_failed";
        return -1;
    }
    if (out_written) *out_written = (size_t)n;
    return 0;
}

/* ── HW_STORAGE: "Console Storage" aggregate ─────────────────────
 *
 * Matches PS5 Settings → Storage → Console Storage:
 *   Total = (/user total - /user reserved) + /system_data total + /system_ex total
 *   Free  = /user bavail + /system_data bavail + /system_ex bavail
 *
 * `bavail` is what the kernel reports as "available to non-root"
 * users; `bfree` is the raw free count. The difference is the
 * "reserved" pool the filesystem keeps so root can still write
 * even when bavail hits zero. Counting reserved as displayable
 * total but not as free matches what Settings shows.
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
    /* Reserved = bfree - bavail (the slice the FS keeps for root).
     * Used for display: total - reserved is what Settings calls
     * "available capacity". */
    uint64_t u_reserved = (u_bfree > u_bavail) ? (u_bfree - u_bavail) : 0;
    uint64_t u_displayable_total = (u_total > u_reserved) ? (u_total - u_reserved) : u_total;

    uint64_t sd_total = 0, sd_bfree = 0, sd_bavail = 0;
    storage_read_part("/system_data", &sd_total, &sd_bfree, &sd_bavail);

    uint64_t sx_total = 0, sx_bfree = 0, sx_bavail = 0;
    storage_read_part("/system_ex", &sx_total, &sx_bfree, &sx_bavail);

    uint64_t total_bytes = u_displayable_total + sd_total + sx_total;
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

int hw_fan_set_threshold(uint8_t threshold_c, const char **err_reason_out) {
    /* Clamp. Intentionally silent — the client UI also clamps, but
     * we enforce here so a malicious/buggy caller can't bypass it by
     * talking FTX2 directly. Out-of-range values get pulled to the
     * nearest safe bound rather than rejected, so the user still gets
     * a working outcome. */
    if (threshold_c < HW_FAN_THRESHOLD_MIN) threshold_c = HW_FAN_THRESHOLD_MIN;
    if (threshold_c > HW_FAN_THRESHOLD_MAX) threshold_c = HW_FAN_THRESHOLD_MAX;

    int fd = open(ICC_FAN_DEVICE_NODE, O_RDONLY);
    if (fd < 0) {
        if (err_reason_out) *err_reason_out = "icc_fan_open_failed";
        return -1;
    }

    unsigned char cmd[ICC_FAN_CMD_LEN] = {0};
    cmd[ICC_FAN_THRESHOLD_OFFSET] = threshold_c;

    int rc = ioctl(fd, ICC_FAN_IOCTL_SET_THRESHOLD, cmd);
    int saved_errno = errno;
    close(fd);

    if (rc < 0) {
        /* On firmwares where the ioctl is refused we keep a generic
         * reason — surfacing errno to the client would leak FreeBSD-
         * specific codes that aren't actionable. Useful local debug
         * info stays in saved_errno for a future logging hook. */
        (void)saved_errno;
        if (err_reason_out) *err_reason_out = "icc_fan_ioctl_failed";
        return -1;
    }
    return 0;
}
