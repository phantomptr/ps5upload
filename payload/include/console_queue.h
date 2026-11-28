/* The console page's queue (#982): game copies and installs added on the PS5 itself, run one
 * after another by the helper, so they finish with the page closed.
 *
 * This is the queue's state and nothing else: what is in it, in which order, how each item ended,
 * kept on disk so a helper restart does not lose it. Starting and watching the jobs is
 * webui_queue.c's; the state never calls out, so it is tested on the host as it is.
 *
 * Items:
 * - copy: `src` (a network share's smb:// path, or a console path) to the console path `dest`.
 * - install: the package at `src`; one on a share is first copied into the install staging
 *   folder (`step` 0, then 1 while the console installs it). The console installs one package at a
 *   time, whoever asked: an install refused as busy waits and is tried again (cq_wait), it does not
 *   fail.
 *
 * Order: strict. The first pending item runs when nothing does and the queue is not paused; one
 * that waits holds the rest. Move to top puts a pending item first. A failed item never stops the
 * queue: the next one starts, and the failure stays listed with its cause until cleared.
 *
 * After a restart (cq_load), an item that was running is marked failed ("interrupted"). When it
 * was an install the queue pauses ("interrupted_install": the console may still be installing it,
 * and nobody knows how that ends), and the page says so and offers Resume; an interrupted copy
 * lets the rest go on.
 *
 * JSON (cq_json): {"paused":bool,"reason":"..","items":[{"id":N,"kind":"copy","state":"pending",
 * "src":"..","dest":"..","name":"..","key":"..","job":"..","error":"..","wait":"..","step":0,
 * "added":T,"started":T,"ended":T,"elapsed_s":N,"bytes_done":N,"bytes_total":N}]} in run order:
 * finished first (oldest first), then the running one, then the pending ones. Times are unix
 * seconds on the console's clock. */
#ifndef PS5UPLOAD_CONSOLE_QUEUE_H
#define PS5UPLOAD_CONSOLE_QUEUE_H

#include <stddef.h>
#include <stdint.h>

#define CQ_MAX 48u
#define CQ_PATH_MAX 1024u
#define CQ_NAME_MAX 160u
#define CQ_KEY_MAX 1024u
#define CQ_JOB_MAX 48u
#define CQ_ERR_MAX 64u
#define CQ_FILE "/data/ps5upload/console-queue.json"

enum { CQ_COPY = 1, CQ_INSTALL = 2 };
enum { CQ_FREE = 0, CQ_PENDING, CQ_RUNNING, CQ_DONE, CQ_FAILED, CQ_CANCELLED };

typedef struct {
    uint32_t id;
    uint8_t kind, state, step, cancel;
    int64_t order;
    char src[CQ_PATH_MAX];
    char dest[CQ_PATH_MAX];
    char name[CQ_NAME_MAX];
    /* The page's own name for what it came from (a library entry), so a game's page shows its
     * own items and nobody else's. */
    char key[CQ_KEY_MAX];
    /* The job running it now ("smb:<id>" or 32 hex), "" between steps. */
    char job[CQ_JOB_MAX];
    /* How it failed (a cause token), or why it waits. */
    char error[CQ_ERR_MAX];
    char wait[CQ_ERR_MAX];
    int64_t added, started, ended;
    uint64_t bytes_done, bytes_total;
    /* Waiting (pending): not tried again before this (monotonic ms). */
    uint64_t retry_at_ms;
} cq_item_t;

/* Add an item; its id (> 0). The same kind and src already pending or running: that one's id.
 * 0 when the queue is full of unfinished items (the oldest finished one makes room otherwise),
 * -1 for a bad request (kind, src, a copy without dest). */
long cq_add(int kind, const char *src, const char *dest, const char *name, const char *key, int64_t now);

/* Cancel item `id`: 1 a pending one was cancelled; 2 the running one is asked to stop (its job,
 * when it has one, copied to `job`: the caller cancels it); 0 not found; -1 it had ended. */
int cq_cancel(uint32_t id, char job[CQ_JOB_MAX]);

/* Put pending item `id` first. 1 moved, 0 not pending. */
int cq_top(uint32_t id);

/* Remove every finished, failed and cancelled item; how many. */
int cq_clear_finished(void);

/* Pause (with a reason token, "" for the user's own) or resume. A pause never stops what runs. */
void cq_set_paused(int paused, const char *reason);
int cq_paused(void);

/* The runner: when nothing runs and the queue is not paused, the first pending item that is not
 * waiting becomes running (started = now) and is copied to `out`: 1. 0 otherwise. */
int cq_take_next(cq_item_t *out, int64_t now, uint64_t now_ms);

/* The running item, copied to `out` (1), or 0. */
int cq_running(cq_item_t *out);

/* Item `id`'s job for its step. */
void cq_set_job(uint32_t id, const char *job, int step);

/* Where running item `id` puts its package while it is copied in from a share (an install). */
void cq_set_dest(uint32_t id, const char *dest);

/* Item `id`'s bytes so far. */
void cq_progress(uint32_t id, uint64_t done, uint64_t total);

/* Item `id` ended: CQ_DONE, CQ_FAILED (with a cause) or CQ_CANCELLED. A running item asked to
 * stop that ends failed is cancelled. */
void cq_finish(uint32_t id, int state, const char *error, int64_t now);

/* Item `id` could not start yet (another install runs on the console): back to pending, first in
 * line, not tried before `at_ms`; `why` is shown meanwhile. A cancel asked meanwhile wins. */
void cq_wait(uint32_t id, const char *why, uint64_t at_ms);

/* Whether anything changed since the last cq_saved() (the caller writes the file then). */
int cq_dirty(void);
void cq_saved(void);

/* The queue as JSON (header comment); its length, or -1 when it does not fit. */
int cq_json(char *out, size_t cap, int64_t now);

/* The queue as the file keeps it (one JSON object per line); its length, or -1. */
int cq_serialize(char *out, size_t cap);

/* Read what cq_serialize wrote, replacing the queue; an item that was running is interrupted
 * (header comment). How many items were read, -1 when the text is not a queue file. */
int cq_deserialize(const char *text, size_t n, int64_t now);

/* Tests only. */
void cq_reset(void);

#endif
