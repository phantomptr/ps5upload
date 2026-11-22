#ifndef PS5UPLOAD2_OWNERSHIP_RECORD_H
#define PS5UPLOAD2_OWNERSHIP_RECORD_H

#include <stddef.h>
#include <stdint.h>

/*
 * The ownership record (/data/ps5upload/runtime/active_instance.txt): which process is the live
 * helper. Review 010 §1 / outage 2026-10-03: a new instance's reap read the prior's record as
 * `started=0` and left a live helper beside it. Root cause, from the code: the reap read the
 * record twice (pid, then started_at_unix) while the HANDING-OVER predecessor, whose listeners had
 * just closed, was unlinking it in its own shutdown (runtime_clear_ownership). A read that lost
 * that race returned 0 for a live instance. So: a record is trusted only when it is complete, a
 * short or missing read is retried once, and a snapshot taken before the takeover backs it up.
 */
typedef struct {
    int pid;               /* 0 = not found */
    uint64_t started;      /* started_at_unix; 0 = not found */
    uint64_t instance_id;  /* 0 = not found */
} ownership_rec_t;

/* Formats a record into buf. -1 when `started` or `pid` is 0 (an instance that has not stamped its
 * start time must never publish a record that reads as unverifiable) or buf is too small; else the
 * length. */
int ownership_record_format(char *buf, size_t n, uint64_t instance_id, int port, int startup_reason,
                            uint64_t started, int pid);

/* Parses record text. Only newline-terminated lines count: a read that caught the file mid-write
 * ends in a partial line (`started_at_unix=17`) that would otherwise parse as a wrong number.
 * Returns 1 when pid and started are both present and nonzero, else 0 (r holds what was found). */
int ownership_record_parse(const char *text, size_t len, ownership_rec_t *r);

/* Reads and parses `path`. When the first read is not complete (missing, empty, partial), waits
 * `retry_ms` and reads once more. Returns 1 when complete. */
int ownership_record_read(const char *path, ownership_rec_t *r, int retry_ms);

/* Fills what a fresh read lost from an earlier snapshot of the SAME instance: when `fresh` has no
 * pid it becomes the snapshot; when the pids agree a missing started/instance_id is taken from the
 * snapshot. A different pid is never mixed in. */
void ownership_record_merge(ownership_rec_t *fresh, const ownership_rec_t *snapshot);

/* Which instance the reap is about (#903). The ownership record (merged with the snapshot taken
 * before the takeover) when it names another process; else the exiting note
 * (<runtime dir>/exiting.txt, written by an instance as its stop begins, same format) when it is
 * complete, written in this boot (started_at_unix >= `boottime`, kern.boottime; 0 = unknown, and
 * then never) and names another process: an instance that removed its record at the end of its
 * stop may still be running, and the new one must wait for it to be gone; else none. `out` gets
 * the one chosen (zeroed for none). */
#define OWNERSHIP_PRIOR_NONE 0
#define OWNERSHIP_PRIOR_RECORD 1
#define OWNERSHIP_PRIOR_EXITING 2
int ownership_prior_pick(const ownership_rec_t *record, const ownership_rec_t *exiting, int exiting_complete,
                         uint64_t boottime, int self_pid, ownership_rec_t *out);

/* 1 when the exiting note is whole and of this boot (see above). */
int ownership_exiting_note_current(const ownership_rec_t *exiting, int exiting_complete, uint64_t boottime);

#endif
