/* AVA1 job journal (SPEC.md §14): one on-disk format, written by the console (C) and
 * the engine (Rust), so either side can resume a job the other started. Filesystem
 * work only — no sockets, no Sony APIs. */
#ifndef AVA1_JOURNAL_H
#define AVA1_JOURNAL_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_gen.h"

#define AVA1_JNL_OPEN 1
#define AVA1_JNL_BATCH 2
#define AVA1_JNL_RESET 3
#define AVA1_JNL_SNAPSHOT 4
#define AVA1_JNL_DONE 5
#define AVA1_JNL_SWEEP 6 /* durable-by-log: files now durable in place (SPEC.md §15.7) */
#define AVA1_JNL_COMPACT_AT (1u << 20)

/* True when `dir` holds a pack log (a file named pack.<n>): durable-by-log's only copy of unswept files. */
int ava1_dir_has_pack(const char *dir);

/* fsync(fd), retried with backoff (20, 60, 200, 600 ms; 5 tries in all) while the error is
 * one a drive can recover from (ava1_fsync_transient). 0, or the errno of the last try.
 * `stopping` (may be NULL) is polled between tries and ends the wait early. *retried is set
 * when a retry is what finally succeeded: the caller may then want to re-read what it
 * synced (SPEC.md §12.6). Every retry is logged to stderr. */
int ava1_fsync_retry(int fd, int (*stopping)(void *), void *arg, int *retried);
/* EINTR, EAGAIN, EBUSY, ETIMEDOUT, ENOENT, ENXIO, ENODEV, also in Sony's 0x8002xxxx form;
 * never EIO, ENOSPC or a read-only drive. */
int ava1_fsync_transient(int e);
/* Tests only: fail the next N ava1_fsync_retry calls (each try counts one) with errno E. */
extern int ava1_fsync_test_fail_n, ava1_fsync_test_errno;
extern unsigned ava1_fsync_retries_total;
extern unsigned ava1_fsync_calls_total;

/* One writer per directory; ava1_jnl_open truncates a torn tail, so it must be the
 * only opener. */
typedef struct {
    int fd;
    uint64_t len;
    char dir[512];
} ava1_jnl_t;

/* Replay visitor: 0 accepts the record; nonzero treats it (and everything after) as torn. */
typedef int (*ava1_jnl_visit_fn)(void *ctx, uint8_t kind, const uint8_t *body, size_t len);

int ava1_jnl_create(ava1_jnl_t *j, const char *dir, const ava1_jnl_open_t *o);
int ava1_jnl_open(ava1_jnl_t *j, const char *dir, ava1_jnl_visit_fn visit, void *ctx);
/* Reads the journal's first record (its JnlOpen) without touching the file: 0, or -1 when
 * there is no intact Open. `o->root` points into `buf`. */
int ava1_jnl_peek_open(const char *dir, uint8_t *buf, size_t cap, ava1_jnl_open_t *o);
int ava1_jnl_append(ava1_jnl_t *j, uint8_t kind, const uint8_t *body, size_t len); /* + fsync */
/* Rewrites the journal as Open ‖ Snapshot ‖ Done. The snapshot carries no terminal
 * status, so pass the Done body (or NULL, 0 when the job is unfinished) — a compaction
 * must never lose state. */
int ava1_jnl_compact(ava1_jnl_t *j, const uint8_t *open_body, size_t open_len,
                     const uint8_t *snap_body, size_t snap_len,
                     const uint8_t *done_body, size_t done_len);
void ava1_jnl_close(ava1_jnl_t *j);
int ava1_manifest_file_write(const char *dir, const uint8_t *blob, size_t len);
int ava1_manifest_file_read(const char *dir, uint8_t **blob, size_t *len); /* malloc'd */
int ava1_jobs_gc(const char *jobs_dir, int64_t now_unix, int64_t max_age_s);
/* Tests only (0 in the payload): a nonzero value replaces the boot identity the GC strikes use. */
extern uint64_t ava1_gc_test_boot_id;
void ava1_job_dir(const char *jobs_dir, const uint8_t job_id[16], char *out, size_t cap);

/* The journal's length, for the receiver's compaction check: compact once it passes
 * AVA1_JNL_COMPACT_AT. */
static inline uint64_t ava1_jnl_len(const ava1_jnl_t *j) { return j->len; }

#endif
