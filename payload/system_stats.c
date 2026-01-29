#include "system_stats.h"

#include <string.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/time.h>
#include <sys/resource.h>
#include <errno.h>
#include <stdio.h>
#include <stdint.h>

#if defined(__FreeBSD__) || defined(__ORBIS__) || defined(__APPLE__)
#include <ifaddrs.h>
#include <net/if.h>
#include <sys/sysctl.h>
#include <sys/user.h>
#endif

#if defined(__ORBIS__)
#include <ps5/kernel.h>

/* PS5-specific memory info functions */
typedef int (*fn_sceKernelGetDirectMemorySize)(uint64_t *size);
typedef int (*fn_sceKernelAvailableDirectMemorySize)(size_t searchStart, size_t searchEnd,
                                                      size_t alignment, size_t *physAddrOut, size_t *sizeOut);
typedef int (*fn_sceKernelGetSystemSwVersion)(void *version);

int sceKernelAvailableFlexibleMemorySize(size_t *out);
int sceKernelGetDirectMemorySize(uint64_t *size);
#endif

#if defined(__ORBIS__)
typedef struct {
    int64_t tv_sec;
    int64_t tv_nsec;
} OrbisKernelTimespec;

typedef struct {
    int32_t lo_data;
    uint32_t td_tid;
    OrbisKernelTimespec user_cpu_usage_time;
    OrbisKernelTimespec system_cpu_usage_time;
} OrbisProcStats;

typedef int (*fn_sceKernelGetCpuUsage)(OrbisProcStats *out, int32_t *size);
typedef int (*fn_sceKernelGetThreadName)(uint32_t id, char *out);
typedef int (*fn_sceKernelClockGettime)(int clockId, OrbisKernelTimespec *tp);
typedef int (*fn_get_page_table_stats)(int vm, int type, int *total, int *free);

int sceKernelLoadStartModule(const char *name, size_t args, const void *argp, uint32_t flags, void *opt, int *res);
int sceKernelDlsym(int handle, const char *symbol, void **addrp);

__attribute__((weak)) int sceKernelGetCpuUsage(OrbisProcStats *out, int32_t *size);
__attribute__((weak)) int sceKernelGetThreadName(uint32_t id, char *out);
__attribute__((weak)) int sceKernelClockGettime(int clockId, OrbisKernelTimespec *tp);
__attribute__((weak)) int get_page_table_stats(int vm, int type, int *total, int *free);
#endif

int get_system_stats(SystemStats *out) {
    if (!out) return -1;
    memset(out, 0, sizeof(*out));
    out->cpu_percent = -1.0;
    out->proc_cpu_percent = -1.0;
    out->rss_bytes = -1;
    out->thread_count = -1;
    out->mem_total_bytes = -1;
    out->mem_free_bytes = -1;
    out->page_size = (int)getpagesize();
    out->net_rx_bytes = -1;
    out->net_tx_bytes = -1;
    out->net_rx_bps = -1;
    out->net_tx_bps = -1;
    out->cpu_supported = 0;
    out->proc_cpu_supported = 0;
    out->rss_supported = 0;
    out->thread_supported = 0;
    out->mem_total_supported = 0;
    out->mem_free_supported = 0;
    out->net_supported = 0;

#if defined(__ORBIS__)
    static int ps5_resolved = -1;
    static fn_sceKernelGetCpuUsage p_get_cpu_usage = NULL;
    static fn_sceKernelGetThreadName p_get_thread_name = NULL;
    static fn_sceKernelClockGettime p_clock_gettime = NULL;
    static fn_get_page_table_stats p_get_page_table_stats = NULL;
    static int logged_ps5_resolve_error = 0;

    if (ps5_resolved == -1) {
        if (!p_get_cpu_usage && sceKernelGetCpuUsage) p_get_cpu_usage = sceKernelGetCpuUsage;
        if (!p_get_thread_name && sceKernelGetThreadName) p_get_thread_name = sceKernelGetThreadName;
        if (!p_clock_gettime && sceKernelClockGettime) p_clock_gettime = sceKernelClockGettime;
        if (!p_get_page_table_stats && get_page_table_stats) p_get_page_table_stats = get_page_table_stats;

        if (!p_get_cpu_usage || !p_get_thread_name || !p_clock_gettime || !p_get_page_table_stats) {
            const char *modules[] = {
                "libkernel.sprx",
                "libkernel_sys.sprx",
                "/system/common/lib/libkernel.sprx",
                "/system/common/lib/libkernel_sys.sprx",
                "/system_ex/common_ex/lib/libkernel.sprx",
                "/system_ex/common_ex/lib/libkernel_sys.sprx"
            };
            for (size_t i = 0; i < sizeof(modules) / sizeof(modules[0]); i++) {
                int handle = sceKernelLoadStartModule(modules[i], 0, NULL, 0, NULL, NULL);
                if (handle < 0) continue;
                if (!p_get_cpu_usage) (void)sceKernelDlsym(handle, "sceKernelGetCpuUsage", (void **)&p_get_cpu_usage);
                if (!p_get_thread_name) (void)sceKernelDlsym(handle, "sceKernelGetThreadName", (void **)&p_get_thread_name);
                if (!p_clock_gettime) (void)sceKernelDlsym(handle, "sceKernelClockGettime", (void **)&p_clock_gettime);
                if (!p_get_page_table_stats) (void)sceKernelDlsym(handle, "get_page_table_stats", (void **)&p_get_page_table_stats);
            }
        }

        if (p_get_cpu_usage && p_get_thread_name && p_clock_gettime) {
            ps5_resolved = 1;
        } else {
            ps5_resolved = 0;
        }
        if (ps5_resolved == 0 && !logged_ps5_resolve_error) {
            logged_ps5_resolve_error = 1;
            printf("[SYS] PS5 symbol resolve failed\n");
        }
    }

    if (ps5_resolved > 0 && p_get_cpu_usage && p_get_thread_name && p_clock_gettime) {
        static uint32_t idle_tids[8] = {0};
        static int idle_tids_valid = 0;
        static int idle_refresh_countdown = 0;
        static long long prev_idle_us[8] = {0};
        static long long prev_wall_us = 0;

        // Static allocation to avoid 180KB stack usage per call
        static OrbisProcStats threads[3072];
        int32_t thread_count = 3072;

        if (p_get_cpu_usage(threads, &thread_count) == 0 && thread_count > 0) {
            out->cpu_supported = 1;
            out->thread_count = (int)thread_count;
            out->thread_supported = 1;

            if (!idle_tids_valid || idle_refresh_countdown <= 0) {
                int found = 0;
                for (int i = 0; i < 8; i++) idle_tids[i] = 0;
                for (int i = 0; i < thread_count; i++) {
                    char name_buf[0x40];
                    int core = -1;
                    if (p_get_thread_name(threads[i].td_tid, name_buf) == 0 &&
                        sscanf(name_buf, "SceIdleCpu%d", &core) == 1 &&
                        core >= 0 && core < 8) {
                        idle_tids[core] = threads[i].td_tid;
                        found++;
                    }
                }
                idle_tids_valid = (found == 8);
                idle_refresh_countdown = 60;
            } else {
                idle_refresh_countdown--;
            }

            if (idle_tids_valid) {
                OrbisKernelTimespec now_ts;
                if (p_clock_gettime(4, &now_ts) == 0) {
                    long long wall_us = (long long)now_ts.tv_sec * 1000000LL + (long long)(now_ts.tv_nsec / 1000LL);
                    if (prev_wall_us > 0) {
                        long long delta_wall = wall_us - prev_wall_us;
                        if (delta_wall > 0) {
                            double total_usage = 0.0;
                            int cores_used = 0;
                            for (int core = 0; core < 8; core++) {
                                uint32_t tid = idle_tids[core];
                                if (!tid) continue;
                                for (int i = 0; i < thread_count; i++) {
                                    if (threads[i].td_tid == tid) {
                                        long long cur_idle_us =
                                            (long long)threads[i].system_cpu_usage_time.tv_sec * 1000000LL +
                                            (long long)(threads[i].system_cpu_usage_time.tv_nsec / 1000LL) +
                                            (long long)threads[i].user_cpu_usage_time.tv_sec * 1000000LL +
                                            (long long)(threads[i].user_cpu_usage_time.tv_nsec / 1000LL);
                                        long long delta_idle = cur_idle_us - prev_idle_us[core];
                                        double idle_frac = (delta_idle >= 0) ? ((double)delta_idle / (double)delta_wall) : 0.0;
                                        if (idle_frac < 0.0) idle_frac = 0.0;
                                        if (idle_frac > 1.0) idle_frac = 1.0;
                                        total_usage += (1.0 - idle_frac) * 100.0;
                                        cores_used++;
                                        prev_idle_us[core] = cur_idle_us;
                                        break;
                                    }
                                }
                            }
                            if (cores_used > 0) {
                                out->cpu_percent = total_usage / (double)cores_used;
                            }
                        }
                    }
                    prev_wall_us = wall_us;
                }
            }
        }
    }

    if (ps5_resolved > 0 && p_get_page_table_stats) {
        int total = 0;
        int free = 0;
        if (p_get_page_table_stats(1, 1, &total, &free) == 0) {
            out->mem_total_bytes = (long long)total * 1024LL * 1024LL;
            out->mem_free_bytes = (long long)free * 1024LL * 1024LL;
            out->mem_total_supported = 1;
            out->mem_free_supported = 1;
        }
    }

    /* Fallback: Try PS5 direct memory functions for total memory */
    if (out->mem_total_bytes < 0) {
        uint64_t direct_mem_size = 0;
        if (sceKernelGetDirectMemorySize(&direct_mem_size) == 0 && direct_mem_size > 0) {
            out->mem_total_bytes = (long long)direct_mem_size;
            out->mem_total_supported = 1;
        }
    }

    /* Fallback: Try flexible memory for free memory estimate */
    if (out->mem_free_bytes < 0) {
        size_t flexible_free = 0;
        if (sceKernelAvailableFlexibleMemorySize(&flexible_free) == 0 && flexible_free > 0) {
            out->mem_free_bytes = (long long)flexible_free;
            out->mem_free_supported = 1;
        }
    }
#endif

#if defined(__FreeBSD__) || defined(__ORBIS__) || defined(__APPLE__)
    static int logged_cp_time_error = 0;
    static int logged_proc_error = 0;
    static int logged_mem_error = 0;
    static int logged_net_error = 0;
    // CPU usage (system-wide) from kern.cp_time
    static long long last_cp_time[5] = {0};
    static int has_prev = 0;
    long cp_time[5] = {0};
    size_t len = sizeof(cp_time);
    if (sysctlbyname("kern.cp_time", &cp_time, &len, NULL, 0) == 0 && len >= sizeof(cp_time)) {
        out->cpu_supported = 1;
        long long total = 0;
        long long idle = 0;
        for (int i = 0; i < 5; i++) total += cp_time[i];
        idle = cp_time[4];
        if (has_prev) {
            long long prev_total = 0;
            long long prev_idle = last_cp_time[4];
            for (int i = 0; i < 5; i++) prev_total += last_cp_time[i];
            long long total_delta = total - prev_total;
            long long idle_delta = idle - prev_idle;
            if (total_delta > 0) {
                out->cpu_percent = 100.0 * (1.0 - ((double)idle_delta / (double)total_delta));
            }
        }
        for (int i = 0; i < 5; i++) last_cp_time[i] = cp_time[i];
        has_prev = 1;
    } else if (!logged_cp_time_error) {
        logged_cp_time_error = 1;
        printf("[SYS] sysctl kern.cp_time failed: %s\n", strerror(errno));
    }

    // Process CPU usage (self)
    static long long last_proc_cpu_us = 0;
    static long long last_proc_time_us = 0;
    struct rusage usage;
    struct timeval now;
    if (getrusage(RUSAGE_SELF, &usage) == 0 && gettimeofday(&now, NULL) == 0) {
        out->proc_cpu_supported = 1;
        long long cpu_us =
            (long long)usage.ru_utime.tv_sec * 1000000LL + (long long)usage.ru_utime.tv_usec +
            (long long)usage.ru_stime.tv_sec * 1000000LL + (long long)usage.ru_stime.tv_usec;
        long long now_us = (long long)now.tv_sec * 1000000LL + (long long)now.tv_usec;
        if (last_proc_time_us > 0) {
            long long delta_cpu = cpu_us - last_proc_cpu_us;
            long long delta_time = now_us - last_proc_time_us;
            if (delta_time > 0 && delta_cpu >= 0) {
                out->proc_cpu_percent = 100.0 * ((double)delta_cpu / (double)delta_time);
            }
        }
        last_proc_cpu_us = cpu_us;
        last_proc_time_us = now_us;
    }

    // Process RSS + thread count
    struct kinfo_proc kp;
    size_t klen = sizeof(kp);
    int mib[4] = { CTL_KERN, KERN_PROC, KERN_PROC_PID, (int)getpid() };
    if (sysctl(mib, 4, &kp, &klen, NULL, 0) == 0 && klen >= sizeof(kp)) {
        if (out->page_size <= 0) out->page_size = 4096;
        out->rss_bytes = (long long)kp.ki_rssize * (long long)out->page_size;
        out->thread_count = (int)kp.ki_numthreads;
        out->rss_supported = 1;
        out->thread_supported = 1;
    } else {
        size_t alt_len = sizeof(kp);
        if (sysctlbyname("kern.proc.pid", &kp, &alt_len, NULL, 0) == 0 && alt_len >= sizeof(kp)) {
            if (out->page_size <= 0) out->page_size = 4096;
            out->rss_bytes = (long long)kp.ki_rssize * (long long)out->page_size;
            out->thread_count = (int)kp.ki_numthreads;
            out->rss_supported = 1;
            out->thread_supported = 1;
        } else if (!logged_proc_error) {
            logged_proc_error = 1;
            printf("[SYS] sysctl kern.proc.pid failed: %s\n", strerror(errno));
        }
    }
    if (out->rss_bytes < 0) {
        // Fallback to max RSS from rusage (FreeBSD returns KB).
        if (getrusage(RUSAGE_SELF, &usage) == 0 && usage.ru_maxrss > 0) {
            out->rss_bytes = (long long)usage.ru_maxrss * 1024LL;
            out->rss_supported = 1;
        }
    }

    // System memory
    uint64_t page_count = 0;
    uint64_t free_count = 0;
    size_t vlen = sizeof(page_count);
    if (sysctlbyname("vm.stats.vm.v_page_count", &page_count, &vlen, NULL, 0) == 0) {
        out->mem_total_bytes = (long long)page_count * (long long)out->page_size;
        out->mem_total_supported = 1;
    }
    vlen = sizeof(free_count);
    if (sysctlbyname("vm.stats.vm.v_free_count", &free_count, &vlen, NULL, 0) == 0) {
        out->mem_free_bytes = (long long)free_count * (long long)out->page_size;
        out->mem_free_supported = 1;
    } else if (!logged_mem_error) {
        logged_mem_error = 1;
        printf("[SYS] sysctl vm.stats.vm.* failed: %s\n", strerror(errno));
    }
    if (out->mem_total_bytes < 0) {
        uint64_t physmem = 0;
        size_t plen = sizeof(physmem);
        if (sysctlbyname("hw.physmem", &physmem, &plen, NULL, 0) == 0 && physmem > 0) {
            out->mem_total_bytes = (long long)physmem;
            out->mem_total_supported = 1;
        } else {
            uint64_t realmem = 0;
            plen = sizeof(realmem);
            if (sysctlbyname("hw.realmem", &realmem, &plen, NULL, 0) == 0 && realmem > 0) {
                out->mem_total_bytes = (long long)realmem;
                out->mem_total_supported = 1;
            } else if (!logged_mem_error) {
                logged_mem_error = 1;
                printf("[SYS] sysctl hw.physmem/hw.realmem failed: %s\n", strerror(errno));
            }
        }
    }

    // Network totals + rate
    static long long last_rx_bytes = 0;
    static long long last_tx_bytes = 0;
    static long long last_net_time_us = 0;
    struct ifaddrs *ifaddr = NULL;
    if (getifaddrs(&ifaddr) == 0 && ifaddr) {
        long long rx_total = 0;
        long long tx_total = 0;
        for (struct ifaddrs *ifa = ifaddr; ifa; ifa = ifa->ifa_next) {
            if (!ifa->ifa_data || !ifa->ifa_name) continue;
            if (ifa->ifa_flags & IFF_LOOPBACK) continue;
            struct if_data *data = (struct if_data *)ifa->ifa_data;
            rx_total += (long long)data->ifi_ibytes;
            tx_total += (long long)data->ifi_obytes;
        }
        freeifaddrs(ifaddr);
        out->net_rx_bytes = rx_total;
        out->net_tx_bytes = tx_total;
        out->net_supported = 1;

        struct timeval net_now;
        if (gettimeofday(&net_now, NULL) == 0) {
            long long now_us = (long long)net_now.tv_sec * 1000000LL + (long long)net_now.tv_usec;
            if (last_net_time_us > 0) {
                long long delta_us = now_us - last_net_time_us;
                long long delta_rx = rx_total - last_rx_bytes;
                long long delta_tx = tx_total - last_tx_bytes;
                if (delta_us > 0 && delta_rx >= 0 && delta_tx >= 0) {
                    out->net_rx_bps = (long long)((double)delta_rx * 1000000.0 / (double)delta_us);
                    out->net_tx_bps = (long long)((double)delta_tx * 1000000.0 / (double)delta_us);
                }
            }
            last_rx_bytes = rx_total;
            last_tx_bytes = tx_total;
            last_net_time_us = now_us;
        }
    } else if (!logged_net_error) {
        logged_net_error = 1;
        printf("[SYS] getifaddrs failed: %s\n", strerror(errno));
    }
#endif

    return 0;
}
