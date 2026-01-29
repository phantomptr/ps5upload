#include "system_stats.h"

#include <string.h>
#include <unistd.h>
#include <sys/types.h>

#if defined(__FreeBSD__) || defined(__ORBIS__) || defined(__APPLE__)
#include <sys/sysctl.h>
#include <sys/user.h>
#endif

int get_system_stats(SystemStats *out) {
    if (!out) return -1;
    memset(out, 0, sizeof(*out));
    out->cpu_percent = -1.0;
    out->rss_bytes = -1;
    out->thread_count = -1;
    out->mem_total_bytes = -1;
    out->mem_free_bytes = -1;
    out->page_size = (int)getpagesize();

#if defined(__FreeBSD__) || defined(__ORBIS__) || defined(__APPLE__)
    // CPU usage (system-wide) from kern.cp_time
    static long long last_cp_time[5] = {0};
    static int has_prev = 0;
    long cp_time[5] = {0};
    size_t len = sizeof(cp_time);
    if (sysctlbyname("kern.cp_time", &cp_time, &len, NULL, 0) == 0 && len >= sizeof(cp_time)) {
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
    }

    // Process RSS + thread count
    struct kinfo_proc kp;
    size_t klen = sizeof(kp);
    int mib[4] = { CTL_KERN, KERN_PROC, KERN_PROC_PID, (int)getpid() };
    if (sysctl(mib, 4, &kp, &klen, NULL, 0) == 0 && klen >= sizeof(kp)) {
        if (out->page_size <= 0) out->page_size = 4096;
        out->rss_bytes = (long long)kp.ki_rssize * (long long)out->page_size;
        out->thread_count = (int)kp.ki_numthreads;
    }

    // System memory
    uint64_t page_count = 0;
    uint64_t free_count = 0;
    size_t vlen = sizeof(page_count);
    if (sysctlbyname("vm.stats.vm.v_page_count", &page_count, &vlen, NULL, 0) == 0) {
        out->mem_total_bytes = (long long)page_count * (long long)out->page_size;
    }
    vlen = sizeof(free_count);
    if (sysctlbyname("vm.stats.vm.v_free_count", &free_count, &vlen, NULL, 0) == 0) {
        out->mem_free_bytes = (long long)free_count * (long long)out->page_size;
    }
#endif

    return 0;
}
