/* Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the
 * Free Software Foundation; either version 3, or (at your option) any
 * later version.
 */

#include "extract_queue.h"
#include "notify.h"
#include "config.h"
#include "transfer.h"
#include "system_stats.h"
#include "third_party/unrar/unrar_wrapper.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
#include <sys/stat.h>
#include <errno.h>
#include <dirent.h>
#include <stdint.h>
#include <stdatomic.h>

static ExtractQueue g_queue;
static pthread_mutex_t g_queue_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_t g_extract_thread;
static atomic_int g_thread_running = 0;         // Atomic for lock-free read
static atomic_long g_last_extract_progress = 0; // Atomic for lock-free read
static atomic_int g_cancel_requested = 0;       // Atomic for lock-free signal
static atomic_int g_requeue_requested = 0;
static atomic_int g_requeue_id = -1;
static time_t g_queue_updated_at = 0;

#define EXTRACT_QUEUE_FILE "/data/ps5upload/extract_queue.bin"
#define EXTRACT_QUEUE_MAGIC 0x31515845 /* 'EXQ1' */
#define EXTRACT_QUEUE_VERSION 2

typedef struct {
    uint32_t magic;
    uint32_t version;
    uint32_t count;
    uint32_t next_id;
    uint64_t updated_at;
} ExtractQueueFileHeader;

/* Notification interval for extraction progress (seconds) */
#define NOTIFY_INTERVAL_SEC 5

/* Forward declarations */
static int chmod_recursive_queue(const char *path, mode_t mode);
static int remove_recursive_queue(const char *path);
static int mkdir_recursive_queue(const char *path);
static void *extract_thread_func(void *arg);
static void extract_queue_touch(void);
static void extract_queue_save_locked(void);
static void extract_queue_load(void);

void extract_queue_init(void) {
    pthread_mutex_lock(&g_queue_mutex);
    memset(&g_queue, 0, sizeof(g_queue));
    g_queue.next_id = 1;
    g_queue.current_index = -1;
    g_queue.server_start_time = time(NULL);
    g_queue_updated_at = time(NULL);
    pthread_mutex_unlock(&g_queue_mutex);
    printf("[EXTRACT_QUEUE] Initialized\n");

    extract_queue_load();
}

static const char *get_archive_name(const char *path) {
    const char *name = strrchr(path, '/');
    return name ? name + 1 : path;
}

int extract_queue_add(const char *source_path, const char *dest_path, int delete_source, const char *cleanup_path, int unrar_mode) {
    if (!source_path || !dest_path) {
        return -1;
    }

    pthread_mutex_lock(&g_queue_mutex);

    for (int i = 0; i < g_queue.count; i++) {
        ExtractQueueItem *existing = &g_queue.items[i];
        if ((existing->status == EXTRACT_STATUS_PENDING || existing->status == EXTRACT_STATUS_RUNNING) &&
            strcmp(existing->source_path, source_path) == 0 &&
            strcmp(existing->dest_path, dest_path) == 0) {
            pthread_mutex_unlock(&g_queue_mutex);
            return -2;
        }
    }

    if (g_queue.count >= EXTRACT_QUEUE_MAX_ITEMS) {
        pthread_mutex_unlock(&g_queue_mutex);
        printf("[EXTRACT_QUEUE] Queue full, cannot add\n");
        return -1;
    }

    ExtractQueueItem *item = &g_queue.items[g_queue.count];
    memset(item, 0, sizeof(*item));

    item->id = g_queue.next_id++;
    strncpy(item->source_path, source_path, sizeof(item->source_path) - 1);
    strncpy(item->dest_path, dest_path, sizeof(item->dest_path) - 1);
    if (cleanup_path && cleanup_path[0]) {
        strncpy(item->cleanup_path, cleanup_path, sizeof(item->cleanup_path) - 1);
    } else {
        item->cleanup_path[0] = '\0';
    }
    item->delete_source = delete_source ? 1 : 0;
    strncpy(item->archive_name, get_archive_name(source_path), sizeof(item->archive_name) - 1);
    if (unrar_mode != EXTRACT_RAR_FAST && unrar_mode != EXTRACT_RAR_SAFE && unrar_mode != EXTRACT_RAR_TURBO) {
        unrar_mode = EXTRACT_RAR_FAST;
    }
    item->unrar_mode = unrar_mode;
    item->status = EXTRACT_STATUS_PENDING;
    item->percent = 0;
    item->processed_bytes = 0;
    item->total_bytes = 0;
    item->files_extracted = 0;
    item->started_at = 0;
    item->completed_at = 0;
    item->error_msg[0] = '\0';

    int id = item->id;
    g_queue.count++;
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);

    printf("[EXTRACT_QUEUE] Added item %d: %s -> %s\n", id, source_path, dest_path);


    return id;
}

static void json_escape_string(const char *src, char *dest, size_t dest_size) {
    size_t di = 0;
    for (size_t si = 0; src[si] && di < dest_size - 1; si++) {
        char c = src[si];
        if (c == '"' || c == '\\') {
            if (di + 2 >= dest_size) break;
            dest[di++] = '\\';
            dest[di++] = c;
        } else if (c == '\n') {
            if (di + 2 >= dest_size) break;
            dest[di++] = '\\';
            dest[di++] = 'n';
        } else if (c == '\r') {
            if (di + 2 >= dest_size) break;
            dest[di++] = '\\';
            dest[di++] = 'r';
        } else if (c == '\t') {
            if (di + 2 >= dest_size) break;
            dest[di++] = '\\';
            dest[di++] = 't';
        } else {
            dest[di++] = c;
        }
    }
    dest[di] = '\0';
}

char *extract_queue_get_status_json(void) {
    pthread_mutex_lock(&g_queue_mutex);

    /* Calculate buffer size needed */
    size_t buf_size = 6144 + (g_queue.count * 2048);
    char *buf = malloc(buf_size);
    if (!buf) {
        pthread_mutex_unlock(&g_queue_mutex);
        return NULL;
    }

    unsigned long uptime = (unsigned long)difftime(time(NULL), g_queue.server_start_time);
    TransferStats transfer_stats;
    transfer_get_stats(&transfer_stats);
    SystemStats sys_stats;
    get_system_stats(&sys_stats);
    time_t last_extract_progress = extract_queue_get_last_progress();

    int pos = snprintf(buf, buf_size,
        "{\"version\":\"%s\",\"uptime\":%lu,\"queue_count\":%d,\"is_busy\":%s,"
        "\"updated_at\":%ld,\"extract_last_progress\":%ld,"
        "\"system\":{\"cpu_percent\":%.2f,\"proc_cpu_percent\":%.2f,\"rss_bytes\":%lld,\"thread_count\":%d,"
        "\"mem_total_bytes\":%lld,\"mem_free_bytes\":%lld,\"page_size\":%d,"
        "\"net_rx_bytes\":%lld,\"net_tx_bytes\":%lld,\"net_rx_bps\":%lld,\"net_tx_bps\":%lld,"
        "\"cpu_supported\":%s,\"proc_cpu_supported\":%s,\"rss_supported\":%s,\"thread_supported\":%s,"
        "\"mem_total_supported\":%s,\"mem_free_supported\":%s,\"net_supported\":%s},"
        "\"transfer\":{\"pack_in_use\":%zu,\"pool_count\":%d,\"queue_count\":%zu,"
        "\"active_sessions\":%d,\"backpressure_events\":%llu,\"backpressure_wait_ms\":%llu,"
        "\"last_progress\":%ld,\"abort_requested\":%s,\"workers_initialized\":%s},\"items\":[",
        PS5_UPLOAD_VERSION, uptime, g_queue.count, g_thread_running ? "true" : "false",
        (long)g_queue_updated_at, (long)last_extract_progress,
        sys_stats.cpu_percent, sys_stats.proc_cpu_percent, sys_stats.rss_bytes, sys_stats.thread_count,
        sys_stats.mem_total_bytes, sys_stats.mem_free_bytes, sys_stats.page_size,
        sys_stats.net_rx_bytes, sys_stats.net_tx_bytes, sys_stats.net_rx_bps, sys_stats.net_tx_bps,
        sys_stats.cpu_supported ? "true" : "false",
        sys_stats.proc_cpu_supported ? "true" : "false",
        sys_stats.rss_supported ? "true" : "false",
        sys_stats.thread_supported ? "true" : "false",
        sys_stats.mem_total_supported ? "true" : "false",
        sys_stats.mem_free_supported ? "true" : "false",
        sys_stats.net_supported ? "true" : "false",
        transfer_stats.pack_in_use, transfer_stats.pool_count, transfer_stats.queue_count,
        transfer_stats.active_sessions,
        (unsigned long long)transfer_stats.backpressure_events,
        (unsigned long long)transfer_stats.backpressure_wait_ms,
        (long)transfer_stats.last_progress,
        transfer_stats.abort_requested ? "true" : "false",
        transfer_stats.workers_initialized ? "true" : "false");
    if (pos < 0 || (size_t)pos >= buf_size) {
        buf[buf_size - 1] = '\0';
        pthread_mutex_unlock(&g_queue_mutex);
        return buf;
    }

    char escaped[EXTRACT_QUEUE_PATH_MAX * 2];

    for (int i = 0; i < g_queue.count && pos < (int)buf_size - 512; i++) {
        ExtractQueueItem *item = &g_queue.items[i];

        if (i > 0) {
            buf[pos++] = ',';
        }

        const char *status_str = "pending";
        switch (item->status) {
            case EXTRACT_STATUS_RUNNING: status_str = "running"; break;
            case EXTRACT_STATUS_COMPLETE: status_str = "complete"; break;
            case EXTRACT_STATUS_FAILED: status_str = "failed"; break;
            default: break;
        }

        json_escape_string(item->archive_name, escaped, sizeof(escaped));

        char src_escaped[EXTRACT_QUEUE_PATH_MAX * 2];
        char dst_escaped[EXTRACT_QUEUE_PATH_MAX * 2];
        json_escape_string(item->source_path, src_escaped, sizeof(src_escaped));
        json_escape_string(item->dest_path, dst_escaped, sizeof(dst_escaped));

        int wrote = snprintf(buf + pos, buf_size - (size_t)pos,
            "{\"id\":%d,\"archive_name\":\"%s\",\"source_path\":\"%s\",\"dest_path\":\"%s\",\"status\":\"%s\","
            "\"percent\":%d,\"processed_bytes\":%llu,\"total_bytes\":%llu,"
            "\"files_extracted\":%d,\"started_at\":%ld,\"completed_at\":%ld",
            item->id, escaped, src_escaped, dst_escaped, status_str,
            item->percent, item->processed_bytes, item->total_bytes,
            item->files_extracted, (long)item->started_at, (long)item->completed_at);
        if (wrote < 0) {
            buf[buf_size - 1] = '\0';
            pthread_mutex_unlock(&g_queue_mutex);
            return buf;
        }
        if ((size_t)wrote >= buf_size - (size_t)pos) {
            pos = (int)buf_size - 1;
            break;
        }
        pos += wrote;

        if (item->error_msg[0]) {
            json_escape_string(item->error_msg, escaped, sizeof(escaped));
            wrote = snprintf(buf + pos, buf_size - (size_t)pos, ",\"error\":\"%s\"", escaped);
            if (wrote < 0) {
                buf[buf_size - 1] = '\0';
                pthread_mutex_unlock(&g_queue_mutex);
                return buf;
            }
            if ((size_t)wrote >= buf_size - (size_t)pos) {
                pos = (int)buf_size - 1;
                break;
            }
            pos += wrote;
        }

        if ((size_t)pos + 1 >= buf_size) {
            pos = (int)buf_size - 1;
            break;
        }
        buf[pos++] = '}';
    }

    if ((size_t)pos < buf_size) {
        int wrote = snprintf(buf + pos, buf_size - (size_t)pos, "]}");
        if (wrote < 0) {
            buf[buf_size - 1] = '\0';
        }
    } else {
        buf[buf_size - 1] = '\0';
    }

    pthread_mutex_unlock(&g_queue_mutex);
    return buf;
}

static int extraction_progress_callback(const char *filename, unsigned long long file_size,
                                        int files_done, unsigned long long total_processed,
                                        unsigned long long total_size, void *user_data) {
    (void)filename;
    (void)file_size;

    // Atomic read - no lock needed for cancel check
    if (atomic_load(&g_cancel_requested)) {
        return 1; /* Stop extraction */
    }

    pthread_mutex_lock(&g_queue_mutex);

    if (g_queue.current_index >= 0 && g_queue.current_index < g_queue.count) {
        ExtractQueueItem *item = &g_queue.items[g_queue.current_index];
        item->processed_bytes = total_processed;
        item->total_bytes = total_size;
        item->files_extracted = files_done;
        item->percent = (total_size > 0) ? (int)((total_processed * 100) / total_size) : 0;
        atomic_store(&g_last_extract_progress, (long)time(NULL));  // Atomic write

        /* Notifications disabled */
    }

    int sleep_needed = 1;
    if (g_queue.current_index >= 0 && g_queue.current_index < g_queue.count) {
        if (g_queue.items[g_queue.current_index].unrar_mode == EXTRACT_RAR_TURBO) {
            sleep_needed = 0;
        }
    }
    pthread_mutex_unlock(&g_queue_mutex);

    if (sleep_needed) {
        usleep(100); /* Yield CPU - reduced from 1000us for better throughput */
    }
    return 0;
}

static void *extract_thread_func(void *arg) {
    (void)arg;

    pthread_mutex_lock(&g_queue_mutex);

    /* Find first pending item */
    int index = -1;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_PENDING) {
            index = i;
            break;
        }
    }

    if (index < 0) {
        g_thread_running = 0;
        pthread_mutex_unlock(&g_queue_mutex);
        return NULL;
    }

    ExtractQueueItem *item = &g_queue.items[index];
    item->status = EXTRACT_STATUS_RUNNING;
    item->started_at = time(NULL);
    g_last_extract_progress = time(NULL);
    g_queue.current_index = index;
    g_cancel_requested = 0;
    extract_queue_touch();

    char source[EXTRACT_QUEUE_PATH_MAX];
    char dest[EXTRACT_QUEUE_PATH_MAX];
    char archive_name[256];
    strncpy(source, item->source_path, sizeof(source) - 1);
    strncpy(dest, item->dest_path, sizeof(dest) - 1);
    strncpy(archive_name, item->archive_name, sizeof(archive_name) - 1);

    pthread_mutex_unlock(&g_queue_mutex);

    printf("[EXTRACT_QUEUE] Starting extraction: %s -> %s\n", source, dest);
    if (item->unrar_mode == EXTRACT_RAR_TURBO) {
        printf("[EXTRACT_QUEUE] Turbo mode: per-file progress updates disabled; totals may be unavailable\n");
    }


    /* Create destination directory */
    if (mkdir_recursive_queue(dest) != 0) {
        pthread_mutex_lock(&g_queue_mutex);
        item->status = EXTRACT_STATUS_FAILED;
        item->completed_at = time(NULL);
        snprintf(item->error_msg, sizeof(item->error_msg), "Create dest failed: %s", strerror(errno));
        g_queue.current_index = -1;
        g_thread_running = 0;
        extract_queue_touch();
        pthread_mutex_unlock(&g_queue_mutex);

        /* keep failed/cancelled archives for requeue; cleanup only on success */

        extract_queue_process();
        return NULL;
    }

    int file_count = 0;
    unsigned long long total_size = 0;
    int use_scan = (item->unrar_mode == EXTRACT_RAR_SAFE);
    if (use_scan) {
        int scan_result = unrar_scan(source, &file_count, &total_size, NULL, 0);
        if (scan_result != UNRAR_OK) {
            pthread_mutex_lock(&g_queue_mutex);
            item->status = EXTRACT_STATUS_FAILED;
            item->completed_at = time(NULL);
            snprintf(item->error_msg, sizeof(item->error_msg), "Scan failed: %s", unrar_strerror(scan_result));
            g_queue.current_index = -1;
            g_thread_running = 0;
            g_requeue_requested = 0;
            g_requeue_id = -1;
            extract_queue_touch();
            pthread_mutex_unlock(&g_queue_mutex);

            /* keep failed/cancelled archives for requeue; cleanup only on success */

            /* Continue with next item */
            extract_queue_process();
            return NULL;
        }

        pthread_mutex_lock(&g_queue_mutex);
        item->total_bytes = total_size;
        pthread_mutex_unlock(&g_queue_mutex);
    }

    /* Extract */
    unrar_extract_opts opts;
    if (item->unrar_mode == EXTRACT_RAR_SAFE) {
        opts.keepalive_interval_sec = UNRAR_SAFE_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_SAFE_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_SAFE_SLEEP_US;
        opts.trust_paths = UNRAR_SAFE_TRUST_PATHS;
        opts.progress_file_start = UNRAR_SAFE_PROGRESS_FILE_START;
    } else if (item->unrar_mode == EXTRACT_RAR_TURBO) {
        opts.keepalive_interval_sec = UNRAR_TURBO_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_TURBO_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_TURBO_SLEEP_US;
        opts.trust_paths = UNRAR_TURBO_TRUST_PATHS;
        opts.progress_file_start = UNRAR_TURBO_PROGRESS_FILE_START;
    } else {
        opts.keepalive_interval_sec = UNRAR_FAST_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_FAST_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_FAST_SLEEP_US;
        opts.trust_paths = UNRAR_FAST_TRUST_PATHS;
        opts.progress_file_start = UNRAR_FAST_PROGRESS_FILE_START;
    }

    int extracted_count = 0;
    unsigned long long extracted_bytes = 0;
    unsigned long long progress_total = use_scan ? total_size : 0;
    int extract_result = unrar_extract(source, dest, 0, progress_total, &opts,
                                       extraction_progress_callback, NULL,
                                       &extracted_count, &extracted_bytes);

    pthread_mutex_lock(&g_queue_mutex);

    if (g_cancel_requested) {
        if (g_requeue_requested && g_requeue_id == item->id) {
            item->status = EXTRACT_STATUS_PENDING;
            item->percent = 0;
            item->processed_bytes = 0;
            item->total_bytes = 0;
            item->files_extracted = 0;
            item->started_at = 0;
            item->completed_at = 0;
            item->error_msg[0] = '\0';
        } else {
            item->status = EXTRACT_STATUS_FAILED;
            snprintf(item->error_msg, sizeof(item->error_msg), "Cancelled");
        }
    } else if (extract_result != UNRAR_OK) {
        item->status = EXTRACT_STATUS_FAILED;
        snprintf(item->error_msg, sizeof(item->error_msg), "Extract failed: %s", unrar_strerror(extract_result));
    } else {
        /* Apply chmod */
        if (chmod_recursive_queue(dest, 0777) != 0) {
            printf("[EXTRACT_QUEUE] Warning: chmod failed for %s\n", dest);
        }

        item->status = EXTRACT_STATUS_COMPLETE;
        item->percent = 100;
        item->files_extracted = extracted_count;
        item->processed_bytes = extracted_bytes;

        /* Notifications disabled */
    }

    if (item->status != EXTRACT_STATUS_PENDING) {
        item->completed_at = time(NULL);
    }
    g_queue.current_index = -1;
    g_thread_running = 0;
    g_requeue_requested = 0;
    g_requeue_id = -1;
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);

    printf("[EXTRACT_QUEUE] Extraction finished for %s\n", source);

    if (item->delete_source && item->status == EXTRACT_STATUS_COMPLETE) {
        unlink(source);
        if (item->cleanup_path[0]) {
            remove_recursive_queue(item->cleanup_path);
        }
    }

    /* Process next item */
    extract_queue_process();

    return NULL;
}

static int mkdir_recursive_queue(const char *path) {
    char tmp[EXTRACT_QUEUE_PATH_MAX];
    char *p = NULL;
    size_t len;

    if (!path || !*path) {
        errno = EINVAL;
        return -1;
    }
    strncpy(tmp, path, sizeof(tmp) - 1);
    tmp[sizeof(tmp) - 1] = '\0';
    len = strlen(tmp);
    if (len == 0) {
        errno = EINVAL;
        return -1;
    }
    if (tmp[len - 1] == '/') {
        tmp[len - 1] = 0;
    }

    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
                return -1;
            }
            chmod(tmp, 0777);
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
        return -1;
    }
    chmod(tmp, 0777);
    return 0;
}

static int remove_recursive_queue(const char *path) {
    DIR *dir = opendir(path);
    if (!dir) {
        return (errno == ENOENT) ? 0 : -1;
    }
    struct dirent *ent;
    char buf[EXTRACT_QUEUE_PATH_MAX];
    while ((ent = readdir(dir)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) {
            continue;
        }
        snprintf(buf, sizeof(buf), "%s/%s", path, ent->d_name);
        struct stat st;
        if (stat(buf, &st) == 0 && S_ISDIR(st.st_mode)) {
            if (remove_recursive_queue(buf) != 0) {
                closedir(dir);
                return -1;
            }
            rmdir(buf);
        } else {
            unlink(buf);
        }
    }
    closedir(dir);
    return rmdir(path);
}

void extract_queue_process(void) {
    pthread_mutex_lock(&g_queue_mutex);

    if (g_thread_running) {
        pthread_mutex_unlock(&g_queue_mutex);
        return;
    }

    /* Check for pending items */
    int has_pending = 0;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_PENDING) {
            has_pending = 1;
            break;
        }
    }

    if (!has_pending) {
        pthread_mutex_unlock(&g_queue_mutex);
        return;
    }

    g_thread_running = 1;

    pthread_mutex_unlock(&g_queue_mutex);

    if (pthread_create(&g_extract_thread, NULL, extract_thread_func, NULL) != 0) {
        pthread_mutex_lock(&g_queue_mutex);
        g_thread_running = 0;
        pthread_mutex_unlock(&g_queue_mutex);
        printf("[EXTRACT_QUEUE] Failed to create extraction thread\n");
        return;
    }

    pthread_detach(g_extract_thread);
}

int extract_queue_is_busy(void) {
    return g_thread_running;
}

int extract_queue_cancel(int id) {
    pthread_mutex_lock(&g_queue_mutex);

    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].id == id) {
            if (g_queue.items[i].status == EXTRACT_STATUS_PENDING) {
                /* Remove pending item */
                g_queue.items[i].status = EXTRACT_STATUS_FAILED;
                strncpy(g_queue.items[i].error_msg, "Cancelled", sizeof(g_queue.items[i].error_msg) - 1);
                g_queue.items[i].completed_at = time(NULL);
                extract_queue_touch();
                pthread_mutex_unlock(&g_queue_mutex);
                return 0;
            } else if (g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
                /* Request cancellation of running extraction */
                g_cancel_requested = 1;
                extract_queue_touch();
                pthread_mutex_unlock(&g_queue_mutex);
                return 0;
            }
            break;
        }
    }

    pthread_mutex_unlock(&g_queue_mutex);
    return -1;
}

int extract_queue_pause(int id) {
    pthread_mutex_lock(&g_queue_mutex);

    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].id == id) {
            if (g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
                g_requeue_requested = 1;
                g_requeue_id = id;
                g_cancel_requested = 1;
                extract_queue_touch();
                pthread_mutex_unlock(&g_queue_mutex);
                return 0;
            }
            if (g_queue.items[i].status == EXTRACT_STATUS_PENDING) {
                extract_queue_touch();
                pthread_mutex_unlock(&g_queue_mutex);
                return 0;
            }
            break;
        }
    }

    pthread_mutex_unlock(&g_queue_mutex);
    return -1;
}

int extract_queue_retry(int id) {
    pthread_mutex_lock(&g_queue_mutex);

    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].id == id) {
            if (g_queue.items[i].status == EXTRACT_STATUS_FAILED) {
                g_queue.items[i].status = EXTRACT_STATUS_PENDING;
                g_queue.items[i].percent = 0;
                g_queue.items[i].processed_bytes = 0;
                g_queue.items[i].total_bytes = 0;
                g_queue.items[i].files_extracted = 0;
                g_queue.items[i].started_at = 0;
                g_queue.items[i].completed_at = 0;
                g_queue.items[i].error_msg[0] = '\0';
                extract_queue_touch();
                pthread_mutex_unlock(&g_queue_mutex);
                return 0;
            }
            break;
        }
    }

    pthread_mutex_unlock(&g_queue_mutex);
    return -1;
}

int extract_queue_remove(int id) {
    pthread_mutex_lock(&g_queue_mutex);
    int index = -1;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].id == id) {
            index = i;
            break;
        }
    }
    if (index == -1) {
        pthread_mutex_unlock(&g_queue_mutex);
        return -1;
    }
    if (g_queue.items[index].status == EXTRACT_STATUS_RUNNING ||
        g_queue.items[index].status == EXTRACT_STATUS_PENDING) {
        pthread_mutex_unlock(&g_queue_mutex);
        return -2;
    }
    for (int i = index; i < g_queue.count - 1; i++) {
        g_queue.items[i] = g_queue.items[i + 1];
    }
    g_queue.count--;
    extract_queue_touch();
    pthread_mutex_unlock(&g_queue_mutex);
    return 0;
}

int extract_queue_count(void) {
    int count = 0;
    pthread_mutex_lock(&g_queue_mutex);
    count = g_queue.count;
    pthread_mutex_unlock(&g_queue_mutex);
    return count;
}

int extract_queue_has_pending(void) {
    int pending = 0;
    pthread_mutex_lock(&g_queue_mutex);
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_PENDING ||
            g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            pending = 1;
            break;
        }
    }
    pthread_mutex_unlock(&g_queue_mutex);
    return pending;
}

void extract_queue_clear_done(void) {
    pthread_mutex_lock(&g_queue_mutex);

    int write_idx = 0;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_PENDING ||
            g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            if (write_idx != i) {
                g_queue.items[write_idx] = g_queue.items[i];
            }
            write_idx++;
        }
    }
    g_queue.count = write_idx;
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);
    printf("[EXTRACT_QUEUE] Cleared completed/failed items, %d remaining\n", write_idx);
}

void extract_queue_clear_all(int keep_running) {
    pthread_mutex_lock(&g_queue_mutex);

    int write_idx = 0;
    for (int i = 0; i < g_queue.count; i++) {
        if (keep_running && g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            if (write_idx != i) {
                g_queue.items[write_idx] = g_queue.items[i];
            }
            write_idx++;
        }
    }
    g_queue.count = write_idx;
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);
    printf("[EXTRACT_QUEUE] Cleared queue, %d remaining\n", write_idx);
}

void extract_queue_clear_failed(void) {
    pthread_mutex_lock(&g_queue_mutex);

    int write_idx = 0;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status != EXTRACT_STATUS_FAILED) {
            if (write_idx != i) {
                g_queue.items[write_idx] = g_queue.items[i];
            }
            write_idx++;
        }
    }
    g_queue.count = write_idx;
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);
    printf("[EXTRACT_QUEUE] Cleared failed items, %d remaining\n", write_idx);
}

void extract_queue_reset(void) {
    pthread_mutex_lock(&g_queue_mutex);
    int running_index = -1;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            running_index = i;
            break;
        }
    }

    if (running_index >= 0) {
        g_cancel_requested = 1;
        ExtractQueueItem running = g_queue.items[running_index];
        g_queue.items[0] = running;
        g_queue.count = 1;
        g_queue.current_index = 0;
    } else {
        g_queue.count = 0;
        g_queue.current_index = -1;
    }
    extract_queue_touch();
    pthread_mutex_unlock(&g_queue_mutex);
}

int extract_queue_reorder(const int *ids, int count) {
    if (!ids || count <= 0) return -1;
    pthread_mutex_lock(&g_queue_mutex);

    if (g_queue.count <= 1) {
        pthread_mutex_unlock(&g_queue_mutex);
        return 0;
    }

    ExtractQueueItem reordered[EXTRACT_QUEUE_MAX_ITEMS];
    int used[EXTRACT_QUEUE_MAX_ITEMS];
    for (int i = 0; i < EXTRACT_QUEUE_MAX_ITEMS; i++) {
        used[i] = 0;
    }

    int write_idx = 0;

    /* Keep running item(s) at the front in original order */
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            reordered[write_idx++] = g_queue.items[i];
            used[i] = 1;
        }
    }

    /* Add items in requested order */
    for (int i = 0; i < count; i++) {
        int id = ids[i];
        for (int j = 0; j < g_queue.count; j++) {
            if (used[j]) continue;
            if (g_queue.items[j].id == id) {
                reordered[write_idx++] = g_queue.items[j];
                used[j] = 1;
                break;
            }
        }
    }

    /* Append remaining items in original order */
    for (int i = 0; i < g_queue.count; i++) {
        if (used[i]) continue;
        reordered[write_idx++] = g_queue.items[i];
    }

    if (write_idx > g_queue.count) {
        write_idx = g_queue.count;
    }

    for (int i = 0; i < write_idx; i++) {
        g_queue.items[i] = reordered[i];
    }
    extract_queue_touch();

    pthread_mutex_unlock(&g_queue_mutex);
    return 0;
}

time_t extract_queue_get_updated_at(void) {
    pthread_mutex_lock(&g_queue_mutex);
    time_t ts = g_queue_updated_at;
    pthread_mutex_unlock(&g_queue_mutex);
    return ts;
}

static void extract_queue_touch(void) {
    g_queue_updated_at = time(NULL);
    extract_queue_save_locked();
}

static void extract_queue_save_locked(void) {
    FILE *fp = fopen(EXTRACT_QUEUE_FILE, "wb");
    if (!fp) {
        return;
    }
    ExtractQueueFileHeader header;
    header.magic = EXTRACT_QUEUE_MAGIC;
    header.version = EXTRACT_QUEUE_VERSION;
    header.count = (uint32_t)g_queue.count;
    header.next_id = (uint32_t)g_queue.next_id;
    header.updated_at = (uint64_t)g_queue_updated_at;
    fwrite(&header, sizeof(header), 1, fp);
    if (g_queue.count > 0) {
        fwrite(g_queue.items, sizeof(ExtractQueueItem), (size_t)g_queue.count, fp);
    }
    fclose(fp);
}

static void extract_queue_load(void) {
    FILE *fp = fopen(EXTRACT_QUEUE_FILE, "rb");
    if (!fp) {
        return;
    }
    ExtractQueueFileHeader header;
    if (fread(&header, sizeof(header), 1, fp) != 1) {
        fclose(fp);
        return;
    }
    if (header.magic != EXTRACT_QUEUE_MAGIC || header.version != EXTRACT_QUEUE_VERSION) {
        fclose(fp);
        return;
    }
    int count = (int)header.count;
    if (count < 0) count = 0;
    if (count > EXTRACT_QUEUE_MAX_ITEMS) count = EXTRACT_QUEUE_MAX_ITEMS;
    if (count > 0) {
        if (fread(g_queue.items, sizeof(ExtractQueueItem), (size_t)count, fp) != (size_t)count) {
            fclose(fp);
            return;
        }
    }
    fclose(fp);

    pthread_mutex_lock(&g_queue_mutex);
    g_queue.count = count;
    g_queue.next_id = (int)header.next_id;
    g_queue_updated_at = (time_t)header.updated_at;
    for (int i = 0; i < g_queue.count; i++) {
        if (g_queue.items[i].status == EXTRACT_STATUS_RUNNING) {
            g_queue.items[i].status = EXTRACT_STATUS_PENDING;
            g_queue.items[i].percent = 0;
            g_queue.items[i].processed_bytes = 0;
            g_queue.items[i].started_at = 0;
        }
    }
    pthread_mutex_unlock(&g_queue_mutex);
}

unsigned long extract_queue_get_uptime(void) {
    return (unsigned long)difftime(time(NULL), g_queue.server_start_time);
}

const ExtractQueueItem *extract_queue_get_current(void) {
    if (g_queue.current_index >= 0 && g_queue.current_index < g_queue.count) {
        return &g_queue.items[g_queue.current_index];
    }
    return NULL;
}

time_t extract_queue_get_last_progress(void) {
    return (time_t)atomic_load(&g_last_extract_progress);  // Lock-free read
}

int extract_queue_is_running(void) {
    return atomic_load(&g_thread_running);  // Lock-free read
}

static int chmod_recursive_queue(const char *path, mode_t mode) {
    struct stat st;
    if (lstat(path, &st) != 0) {
        return -1;
    }

    if (chmod(path, mode) != 0) {
        return -1;
    }

    if (S_ISDIR(st.st_mode)) {
        DIR *dir = opendir(path);
        if (!dir) {
            return -1;
        }
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            char child[EXTRACT_QUEUE_PATH_MAX];
            snprintf(child, sizeof(child), "%s/%s", path, entry->d_name);
            chmod_recursive_queue(child, mode);
        }
        closedir(dir);
    }

    return 0;
}
