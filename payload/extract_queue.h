/* Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the
 * Free Software Foundation; either version 3, or (at your option) any
 * later version.
 */

#ifndef EXTRACT_QUEUE_H
#define EXTRACT_QUEUE_H

#include <stdint.h>
#include <time.h>

#define EXTRACT_QUEUE_MAX_ITEMS 16
#define EXTRACT_QUEUE_PATH_MAX 1024

typedef enum {
    EXTRACT_STATUS_PENDING = 0,
    EXTRACT_STATUS_RUNNING = 1,
    EXTRACT_STATUS_COMPLETE = 2,
    EXTRACT_STATUS_FAILED = 3
} ExtractStatus;

/* Match UnrarMode values in unrar_handler.h */
typedef enum {
    EXTRACT_RAR_FAST = 0,
    EXTRACT_RAR_SAFE = 1,
    EXTRACT_RAR_TURBO = 2
} ExtractRarMode;

typedef struct {
    int id;
    char source_path[EXTRACT_QUEUE_PATH_MAX];
    char dest_path[EXTRACT_QUEUE_PATH_MAX];
    char cleanup_path[EXTRACT_QUEUE_PATH_MAX];
    int delete_source;
    char archive_name[256];
    int unrar_mode;
    ExtractStatus status;
    int percent;
    unsigned long long processed_bytes;
    unsigned long long total_bytes;
    int files_extracted;
    time_t started_at;
    time_t completed_at;
    char error_msg[256];
} ExtractQueueItem;

typedef struct {
    ExtractQueueItem items[EXTRACT_QUEUE_MAX_ITEMS];
    int count;
    int next_id;
    int current_index;
    time_t server_start_time;
} ExtractQueue;

/* Initialize the extraction queue */
void extract_queue_init(void);

/* Add an item to the extraction queue, returns item id or -1 on error, -2 on duplicate */
int extract_queue_add(const char *source_path, const char *dest_path, int delete_source, const char *cleanup_path, int unrar_mode);

/* Get current queue status as JSON string (caller must free) */
char *extract_queue_get_status_json(void);

/* Process the next pending item in queue (non-blocking check, blocking extract) */
void extract_queue_process(void);

/* Check if queue is currently processing */
int extract_queue_is_busy(void);

/* Cancel a queue item by id, returns 0 on success */
int extract_queue_cancel(int id);
int extract_queue_pause(int id);
int extract_queue_retry(int id);
int extract_queue_remove(int id);
int extract_queue_count(void);
int extract_queue_has_pending(void);

/* Clear completed/failed items from queue */
void extract_queue_clear_done(void);
void extract_queue_clear_all(int keep_running);
void extract_queue_clear_failed(void);
void extract_queue_reset(void);

/* Get last update time */
time_t extract_queue_get_updated_at(void);

/* Reorder queue items by ID list */
int extract_queue_reorder(const int *ids, int count);

/* Get server uptime in seconds */
unsigned long extract_queue_get_uptime(void);

/* Get current running item progress for notifications */
const ExtractQueueItem *extract_queue_get_current(void);

/* Get last progress time and running state */
time_t extract_queue_get_last_progress(void);
int extract_queue_is_running(void);

#endif /* EXTRACT_QUEUE_H */
