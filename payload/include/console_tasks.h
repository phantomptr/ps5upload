/* What the console page started, for the apps connected to this console (#658).
 *
 * The page's jobs run in the helper (AVA1 jobs under the page's own identity), so any app can read
 * them. A paired app's job.list only shows its own, and the job table does not know what a job is
 * working on, and forgets it soon after it ends: a cancelled copy leaves the list at once and a
 * finished one 10 s after the open page first saw it, which an app reading every 10 s would miss.
 * So the page's routes note each job here (what it is, its name); every listing updates it, and it
 * keeps how a job ended for CTASKS_KEEP_MS.
 *
 * Only jobs noted here are ever listed or cancellable through it: an app's own job never is, so it
 * cannot show twice in that app and no paired PC can stop another one's job.
 *
 * One entry of the JSON: {"id":"<32 hex>","kind":"copy","state":"running","name":"game.iso",
 * "files_done":N,"files_total":N,"bytes_done":N,"bytes_total":N,"age_s":N}. kind: the token given to
 * ctasks_started. state: running, done, failed, cancelled, or gone (it left the job table without
 * anyone seeing how it ended). age_s: seconds since it ended, 0 while it runs. name may be "". */
#ifndef PS5UPLOAD_CONSOLE_TASKS_H
#define PS5UPLOAD_CONSOLE_TASKS_H

#include <stddef.h>
#include <stdint.h>

/* AVA1_MAX_JOBS can run at once; the rest of the slots hold what recently ended. */
#define CTASKS_SLOTS 48u
#define CTASKS_KEEP_MS (5u * 60u * 1000u)
/* A job this new may not be in a listing that was taken just before it started. */
#define CTASKS_NEW_MS 2000u

/* The page started job `id`: kind is a short token (copy, move, delete, ...), path what it works on
 * (only its last name is kept). Reuses the slot that ended longest ago. */
void ctasks_started(const uint8_t id[16], const char *kind, const char *path, uint64_t now_ms);

/* Whether `id` is a job the page started. */
int ctasks_known(const uint8_t id[16]);

/* Stop was asked for: when it ends with an error (or leaves the list), it was cancelled. 0 when it
 * is not running any more: it had ended by itself, and nothing is asked. */
int ctasks_cancel_requested(const uint8_t id[16]);

/* The stop did not go through: how the job ends from here is how it ended by itself. */
void ctasks_cancel_failed(const uint8_t id[16]);

/* A JobListResult of every job (ava1_jobs_list_all) brings the noted ones up to date. -1 when it
 * cannot be read. */
int ctasks_observe(const uint8_t *res, size_t n, uint64_t now_ms);

/* The entries still kept, as JSON array elements with no brackets, into out. Their length, or -1
 * when they do not fit. */
int ctasks_json(char *out, size_t cap, uint64_t now_ms);

/* Tests only. */
void ctasks_reset(void);

#endif
