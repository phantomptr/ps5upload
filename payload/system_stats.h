#ifndef SYSTEM_STATS_H
#define SYSTEM_STATS_H

#include <stdint.h>

typedef struct {
    double cpu_percent;
    long long rss_bytes;
    int thread_count;
    long long mem_total_bytes;
    long long mem_free_bytes;
    int page_size;
} SystemStats;

int get_system_stats(SystemStats *out);

#endif
