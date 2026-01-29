#ifndef SYSTEM_STATS_H
#define SYSTEM_STATS_H

#include <stdint.h>

typedef struct {
    double cpu_percent;
    double proc_cpu_percent;
    long long rss_bytes;
    int thread_count;
    long long mem_total_bytes;
    long long mem_free_bytes;
    int page_size;
    long long net_rx_bytes;
    long long net_tx_bytes;
    long long net_rx_bps;
    long long net_tx_bps;
    int cpu_supported;
    int proc_cpu_supported;
    int rss_supported;
    int thread_supported;
    int mem_total_supported;
    int mem_free_supported;
    int net_supported;
} SystemStats;

int get_system_stats(SystemStats *out);

#endif
