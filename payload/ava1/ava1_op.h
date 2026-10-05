/* AVA1 long-running management operations as jobs (P3 Task 5; SPEC.md section 7.3, job.run).
 *
 * `job.run{job_id, op, args}` starts an operation on its own worker thread (512 KiB stack,
 * the management stack) and answers at once with a Status. The operation reports progress
 * through its context; `job.status` returns it (ext `result` and `code` once finished) and
 * `job.cancel` raises the context's cancel flag. The job is an ordinary entry of the job
 * table: it counts against AVA1_MAX_JOBS, is owned by the peer that started it, has no
 * session (so a reconnect does not matter) and is collected by the reaper a park age
 * after it ended, exactly like a console-local copy.
 *
 * Operations are registered by the embedder: the pure filesystem ones in payload/src/fs_jobs.c
 * and the ones that wrap a management handler through payload/src/mgmt_table.def.
 */
#ifndef AVA1_OP_H
#define AVA1_OP_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_job.h"

/* JobEntry.kind of an operation job (the schema's kinds stop at JOB_COPY = 3). */
#define AVA1_JOB_OPKIND 4
/* Most operations running at once; one more is ERR_BUSY. */
#define AVA1_OP_MAX_RUNNING 8
/* The largest result body a job keeps (a Status reply carries it on every poll). */
#define AVA1_OP_RESULT_MAX (128u * 1024u)
#define AVA1_OP_ARGS_MAX (60u * 1024u)

typedef struct ava1_op_ctx ava1_op_ctx_t;

/* An operation: returns AVA1_STATUS_OK, or an AVA1_ERR_* with the cause set through
 * ava1_op_message(). A result body is set through ava1_op_set_result(). */
typedef int (*ava1_op_fn)(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t args_len);

/* Registers (or, with the same `op`, replaces) an operation. 0, or -1 when the table is full. */
int ava1_op_register(uint8_t op, ava1_op_fn fn, void *arg);
void ava1_op_unregister_all(void);

/* ---- for operations ---- */
int ava1_op_cancelled(const ava1_op_ctx_t *c);                      /* 1 when job.cancel arrived */
void ava1_op_set_total(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes);
void ava1_op_add(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes); /* progress, monotonic */
void ava1_op_message(ava1_op_ctx_t *c, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
/* Copies `len` bytes as the job's result. 0, or -1 when it is larger than AVA1_OP_RESULT_MAX. */
int ava1_op_set_result(ava1_op_ctx_t *c, const void *body, size_t len);

/* ---- for the data layer ---- */
/* job.run: the reply body (a Status) or the cause goes to out. Returns an AVA1 status. */
int ava1_op_run_rpc(const uint8_t *body, uint32_t len, const uint8_t owner[32], uint8_t *out, size_t cap,
                    size_t *out_len);
/* job.list: the peer's jobs of every kind as a JobListResult. */
int ava1_op_list_rpc(const uint8_t owner[32], uint8_t *out, size_t cap, size_t *out_len);
/* Encodes the Status of an operation job (job.status / job.run reply). */
int ava1_op_encode_status(ava1_job_t *j, uint8_t *out, size_t cap, size_t *out_len);
/* job.cancel on an operation job: raises the flag and returns (never waits); the worker stops at
 * its next check and the job stays listed (finished, ERR_CANCELLED) so a poller reads how it ended. */
void ava1_op_cancel(ava1_job_t *j);
/* A status reply was produced for `j`: when it was terminal and the operation is repeatable, the
 * job stays listed for a grace (AVA1_OP_GRACE_MS) after the FIRST such reply, so a lost reply still
 * gets the stored answer and a repeat job.run does not re-run it; it is then released by the reaper
 * or by the next read after the grace, and at once (the oldest first) when the table is full. `was_finished` is
 * `ava1_op_finished_before(j)` read BEFORE the reply was encoded: an operation that finished while
 * a "running" reply was being built must stay listed, or the next job.status finds no job. */
int ava1_op_finished_before(ava1_job_t *j);
/* Test seam (NULL in the payload): called between that read and the encoding of job.run's reply. */
extern void (*ava1_op_test_pre_encode)(ava1_job_t *j);
void ava1_op_status_delivered(ava1_job_t *j, int was_finished);
/* How long a finished operation that is not released on read stays listed (ms). */
#define AVA1_OP_DONE_AGE_MS 30000u
/* How long a releasable operation stays listed after its terminal status was first delivered (ms). */
#define AVA1_OP_GRACE_MS 10000u
/* The reaper's keep time for a finished operation (ms): the grace once delivered, else the done-age;
 * both are capped by the configured park age. */
uint64_t ava1_op_keep_ms(int delivered);

#endif
