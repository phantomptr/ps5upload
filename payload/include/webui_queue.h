/* The console page's queue runner (#982): runs console_queue.h's items one after another on a
 * thread of its own, through the same routes the page calls (webui_api_call), so a queued copy or
 * install is checked and started exactly as one the page starts by hand, and shows in Tasks and in
 * the apps like one. The queue is kept in CQ_FILE.
 *
 * Routes (POST, JSON): /api/queue -> the queue (console_queue.h cq_json);
 * /api/queue/add {kind:"copy"|"install", src, dest?, name?, key?} -> {id};
 * /api/queue/cancel {id}; /api/queue/top {id}; /api/queue/clear; /api/queue/pause {paused}. */
#ifndef PS5UPLOAD_WEBUI_QUEUE_H
#define PS5UPLOAD_WEBUI_QUEUE_H

#include <stddef.h>
#include <stdint.h>

/* Reads the saved queue and starts the runner, once (main.c at start; any queue route too). */
void webui_queue_start(void);

void webui_queue_get(const char *b, size_t n, uint8_t *out, int fd);
void webui_queue_add(const char *b, size_t n, uint8_t *out, int fd);
void webui_queue_cancel(const char *b, size_t n, uint8_t *out, int fd);
void webui_queue_top(const char *b, size_t n, uint8_t *out, int fd);
void webui_queue_clear(const char *b, size_t n, uint8_t *out, int fd);
void webui_queue_pause(const char *b, size_t n, uint8_t *out, int fd);

/* The queue's JSON for the apps (console tasks); its length, or -1. */
int webui_queue_json(char *out, size_t cap);

#endif
