/* AVA1 apply engine (SPEC.md §12.5, §12.6, §13, §14): the receiver's work after the map.
 * Chunks and bundles go to a worker pool; the job thread batches fsyncs, journals them,
 * acknowledges Durable, commits large files whose root matches, and finishes the job. */
#ifndef AVA1_APPLY_H
#define AVA1_APPLY_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_gen.h"
#include "ava1_job.h"

#define AVA1_W_CHUNK 1
#define AVA1_W_BUNDLE 2
#define AVA1_W_CALL 3
#define AVA1_W_SWEEP 5  /* durable-by-log: make logged small files durable in place (SPEC.md §15.7) */
#define AVA1_W_COMMIT 4 /* commit_large of one file (review 003 §3.3), run by a worker */
#define AVA1_PEND_MAX 512u          /* small-file fds held open until their batch */
#define AVA1_CRASH_AFTER_SYNC 1     /* tests: die after fsync, before the journal */
#define AVA1_CRASH_AFTER_JOURNAL 2  /* tests: die after the journal, before Durable */
#define AVA1_CRASH_AFTER_TAKE 3     /* tests: die after taking <root>, before the journal (Task 13) */
#define AVA1_CRASH_COMMIT_RENAMED 4 /* tests: die after a commit's rename + dir sync, before its journal */
#define AVA1_CRASH_BEFORE_COMMIT 5  /* tests: die as a fully durable file's commit starts */
#define AVA1_CRASH_STAGED_RENAMED 6 /* tests: die after the staging rename + sync, before Done */
#define AVA1_CRASH_MID_REMAP 7      /* tests: die after a remap's outboard renames */
#define AVA1_CRASH_AFTER_DATA 8     /* tests: die after the batch's data fsync, before any directory sync */
#define AVA1_CRASH_MID_SWEEP 11     /* tests: die in a sweep, after its files and directories are synced, before JnlSweep */
#define AVA1_CRASH_AFTER_SWEEP 10   /* tests: die after a JnlSweep is journaled, before its pack segment is deleted */
#define AVA1_CRASH_MID_DIRS 9       /* tests: die once the first of a batch's directories is synced */

/* `owned` buffers are freed by the apply engine (always, including on error); their
 * length returns to the sender as credit. */
int ava1_apply_start(ava1_job_t *j);                    /* job thread + workers_start workers */
int ava1_apply_reserve(ava1_job_t *j, size_t n);       /* credit: 0, or -1 when it would exceed */
void ava1_apply_unreserve(ava1_job_t *j, size_t n);
/* AVA1_E_PROTO unless the chunk is inside a file and group-aligned: every chunk but a
 * file's final one is a whole number of verification groups. */
int ava1_apply_chunk(ava1_job_t *j, uint8_t *owned, size_t owned_len, uint32_t file_id, uint64_t off,
                     const uint8_t *data, size_t len);
int ava1_apply_bundle(ava1_job_t *j, uint8_t *owned, size_t owned_len, const ava1_bundle_t *b);
/* The same, for an `owned` buffer from ava1_frame_alloc: `owned_cap` is the capacity it
 * returned and the buffer goes back to the pool, not the allocator (review 003 section 3). */
int ava1_apply_chunk_pooled(ava1_job_t *j, uint8_t *owned, size_t owned_len, size_t owned_cap, uint32_t file_id,
                            uint64_t off, const uint8_t *data, size_t len);
int ava1_apply_bundle_pooled(ava1_job_t *j, uint8_t *owned, size_t owned_len, size_t owned_cap,
                             const ava1_bundle_t *b);
int ava1_apply_root(ava1_job_t *j, uint32_t file_id, const uint8_t root[32]);
/* Runs fn(j, arg, 0..n-1) on the workers and waits for all of them. */
int ava1_apply_parallel(ava1_job_t *j, void (*fn)(ava1_job_t *, void *, uint32_t), void *arg, uint32_t n);
void ava1_apply_path(const ava1_job_t *j, uint32_t id, int part, char *out, size_t cap);
/* Ends the job once (also the success path, status AVA1_STATUS_OK): journal Done when
 * asked, then JobDone. */
/* The only way to append to a job's journal once workers run (they append commits): the
 * appends are serialised on the job. Returns ava1_jnl_append's. */
int ava1_apply_jnl_append(ava1_job_t *j, uint8_t kind, const uint8_t *body, size_t len);
void ava1_apply_fail(ava1_job_t *j, uint16_t status, const char *what, int err, int journal_done);
void ava1_apply_status(ava1_job_t *j);                  /* emit one Status now */
/* Forget a large file's progress (journal Reset, clear its ranges and root, blank its
 * outboard). Requires j->lf[id] != NULL. `reason == 0` resets silently (no FileRetry). */
void ava1_apply_reset(ava1_job_t *j, uint32_t id, uint16_t reason);
/* Rewrite the journal as Open + Snapshot of the job's state (mid-job: no Done). */
/* 0 when compacted, -1 when skipped (a commit is in flight, or out of memory: it is retried). */
int ava1_apply_compact(ava1_job_t *j);
/* Job thread only: waits until no work is queued or running, then makes what was applied
 * durable (one sync batch) and drops what could not be, so the manifest can change. */
int ava1_apply_quiesce(ava1_job_t *j); /* 0, or an errno when the unswept files could not be made durable */
/* Queues the commit of every large file whose ranges and root are all durable onto the
 * workers and returns at once (a commit is four fsyncs; it must not stop the job thread). */
void ava1_apply_commit_ready(ava1_job_t *j);
/* One line: where the job's time went, as a share of its wall time (scan, data fsync, directory
 * fsyncs, journal on the job thread; commit and preallocate summed over workers, so they can pass
 * 100% together). Printed once when the job ends, always. Returns the length written. */
size_t ava1_apply_summary(const ava1_job_t *j, char *out, size_t cap);
/* Durable-by-log: re-materialises and sweeps the files `unswept` (bits over file ids) still names, from
 * the pack ranges `refs`; files whose record is gone are reset (JnlReset). Run on JobOpen and at start;
 * leaves no pack segment behind when it succeeds. 0, or -1 when the journal cannot be written. */
int ava1_pack_recover(ava1_job_t *j, const ava1_bits_t *unswept, const ava1_pack_ref_t *refs, uint32_t nrefs);
/* Makes every logged file durable in place now (a changed manifest, a staged job's final rename).
 * 0, or an errno. */
int ava1_pack_drain(ava1_job_t *j);
/* Sends a finished job's JobDone again (a sender that lost the first one asks again). */
void ava1_apply_done_again(ava1_job_t *j);
/* The staged tree was renamed before a crash or sync failure: sync its parent and
 * journal a fresh Done before a resumed move can remove its source. */
void ava1_apply_finish_landed(ava1_job_t *j);

/* Tests only (NULL in the payload): called at these points of a commit, in this order.
 * `id` is the file, or UINT32_MAX for the staged tree's rename. */
#define AVA1_HOOK_COMMIT_VERIFIED 1 /* root matched; the commit is about to proceed */
#define AVA1_HOOK_RENAMED 2         /* the rename into place */
#define AVA1_HOOK_DIR_SYNCED 3      /* its directory fsynced */
#define AVA1_HOOK_JOURNALED 4       /* the file's done Batch appended */
#define AVA1_HOOK_OB_UNLINKED 5     /* its outboard removed */
/* And at these points of every sync batch (id UINT32_MAX, or a file in the directory). */
#define AVA1_HOOK_BATCH_SYNCED 6    /* the batch's file data fsynced */
#define AVA1_HOOK_BATCH_DIR_SYNCED 7 /* one directory that gained an entry, fsynced */
#define AVA1_HOOK_BATCH_JOURNALED 8 /* the batch appended to the journal */
#define AVA1_HOOK_PREP_DIR_SYNCED 10 /* one directory prepare synced (job thread's wait, workers' sync) */
#define AVA1_HOOK_SWEEP_FILE 12     /* a sweep is about to sync file `id`: ava1_apply_fault may fail it (tests) */
#define AVA1_HOOK_SWEPT 11          /* a JnlSweep was appended (the files are durable in place) */
#define AVA1_HOOK_BATCH_ALLOC 13    /* a sync batch is about to allocate its work lists: a nonzero ava1_apply_fault is "out of memory" (tests) */
#define AVA1_HOOK_PREALLOC 9        /* a part file is about to be preallocated (job mutex NOT held) */
extern void (*ava1_apply_hook)(ava1_job_t *j, int point, uint32_t id);
/* Tests only (NULL in the payload): an errno to inject at a point instead of doing the work.
 * Consulted at AVA1_HOOK_DIR_SYNCED of the staged tree's rename. */
extern int (*ava1_apply_fault)(ava1_job_t *j, int point, uint32_t id);
/* Tests only (0 in the payload): while nonzero, the job thread starts no sync batch. */
extern int ava1_apply_hold_batches;

#endif
