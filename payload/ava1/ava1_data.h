/* AVA1 data-layer config, credit budget and housekeeping (SPEC.md §13, §15). */
#ifndef AVA1_DATA_H
#define AVA1_DATA_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_job.h"
#include "ava1_server.h"

typedef struct {
    char jobs_dir[256];
    uint64_t budget;                      /* credit across all jobs; 0 = 96 MiB */
    uint8_t workers_start, workers_min, workers_max; /* 0 = 4, 2, 16 */
    /* small/large (SPEC.md §12.2); 0 = 256 KiB. JobOpen carries no cutoff, so the sender
     * must use this one: a Chunk for a smaller file, or a BundleRecord for a file this
     * size or larger, ends the job with ERR_PROTOCOL. */
    uint32_t cutoff;
    int (*may_write)(const char *abs);    /* 1 = allowed */
    int (*may_read)(const char *abs, int unsafe_read);
    int (*same_device)(const char *a, const char *b); /* 1 same, 0 crosses, -1 unknown */
    /* Review S2: 1 = `abs` (a symlink met while walking a download/copy source) leads to something the data
     * plane must never read: the trust store, or an ancestor of it. The walk skips such an entry and the open
     * refuses it. May be NULL. */
    int (*refuse_link)(const char *abs);
    uint32_t fsync_delay_us;              /* tests: a slow disk */
    int crash_at;                         /* tests: AVA1_CRASH_* (Task 13) */
    uint32_t park_ms;                     /* a parked job is freed after this; 0 = AVA1_PARK_MS */
    /* Control-frame bytes queued for jobs (their inboxes and the frames behind a JobOpen
     * still opening), all jobs together; 0 = 64 MiB. Past it, the job ends with ERR_BUSY. */
    uint32_t ctl_cap;
    /* Durable-by-log for small files (SPEC.md §15.7). 0 = the default (on), AVA1_LOG_SMALL_ON,
     * AVA1_LOG_SMALL_OFF: the per-file-fsync path, kept for one release. */
    uint8_t log_small;
    uint32_t pack_segment;                /* bytes per pack segment; 0 = 64 MiB */
    uint64_t unswept_max;                 /* pack bytes whose files are not yet durable in place; 0 = 256 MiB */
    uint32_t sweep_age_ms;                /* a batch's files are swept after this; 0 = 3000 (tests set 1) */
    uint64_t unswept_total;               /* the same cap across all jobs; 0 = 512 MiB */
    uint32_t recover_every_ms;            /* housekeeping recovers parked/crashed job dirs this often; 0 = 10000 */
    uint32_t recover_max;                 /* job directories one recovery pass takes; 0 = 4 */
    /* Review 006 #2: a receiving job that makes no progress for this long while its sender still owes
     * bytes (it heartbeats but sends no data) is ended with AVA1_ERR_STALLED. 0 = AVA1_PROGRESS_MS. A job that
     * resumed (its journal held done or partial files when it opened or reattached) uses resume_progress_ms. */
    uint32_t progress_ms;
    uint32_t resume_progress_ms;          /* the same for a resumed job; 0 = AVA1_RESUME_PROGRESS_MS */
} ava1_data_cfg_t;
/* 3 x dead_after (12 s): generous, so a slow-but-moving link or drive is never cut. A resumed job may
 * spend a long time with the sender hashing durable groups it will not resend (no frame), so it waits
 * far longer. */
#define AVA1_PROGRESS_MS 36000u
#define AVA1_RESUME_PROGRESS_MS 900000u
#define AVA1_LOG_SMALL_ON 1
#define AVA1_LOG_SMALL_OFF 2
#define AVA1_PACK_SEGMENT (64u << 20)
#define AVA1_UNSWEPT_MAX (256ull << 20)
#define AVA1_UNSWEPT_TOTAL (512ull << 20)
#define AVA1_SWEEP_AGE_MS 3000u
/* 1 when small files go through the pack log (the data layer's effective setting). */
/* Runtime off-switch (review 007 #5): when this file exists the console takes the per-file fsync path
 * for every job OPENED from then on (a job decides once, at open, and never switches mid-job).
 * Recovery of already-logged jobs ignores it. Same directory as the timing flag. */
#define AVA1_LOG_SMALL_OFF_FLAG "/data/ps5upload/debug/ava1-log-small-off"
extern const char *ava1_log_small_flag_path; /* AVA1_LOG_SMALL_OFF_FLAG; a test points it elsewhere */
int ava1_data_log_small(void);               /* the answer a job opened now would take: 1 logged, 0 per-file */
int ava1_data_log_small_flagged(void);       /* 1 when the debug flag file is present */
/* Crash recovery of durable-by-log (SPEC.md §15.7): one pass over the jobs directory takes up to `max`
 * job directories that hold a pack log and nobody has open, re-makes their unswept files from the log and
 * sweeps them. Run at start and then by housekeeping, so a job that was reaped or crashed is finished
 * without a JobOpen. Returns how many it took. */
uint32_t ava1_recv_recover_pass(const char *jobs_dir, uint32_t max);
/* Pack bytes of files not yet swept, all jobs together (the cross-job cap's counter). */
void ava1_unswept_add(int64_t delta);
uint64_t ava1_unswept_total(void);
/* 0 once ava1_data_stop has begun: long background work (recovery) checks it and returns. */
int ava1_data_running(void);
/* Housekeeping loop iterations since start (tests: a slow recovery must not stall the reaper). */
extern unsigned ava1_house_ticks;

int ava1_data_start(const ava1_data_cfg_t *cfg);  /* starts housekeeping; 0 or -errno */
void ava1_data_stop(void);                         /* stops and frees every job */
const ava1_data_cfg_t *ava1_data_cfg(void);
uint64_t ava1_budget_take(uint64_t want, uint64_t min); /* grants up to want, 0 if < min free */
void ava1_budget_give(uint64_t n);
/* Open-file budget (derived from RLIMIT_NOFILE at ava1_data_start: soft limit - 128, 512
 * when unreadable). The apply engine's pending small-file descriptors, all jobs together,
 * stay within ava1_pend_share() (half of it); disk.calibrate holds at most that many. */
int ava1_data_boot_recovered(void); /* the start-time recovery pass has run */
uint32_t ava1_fd_budget(void);
/* Reads and raises RLIMIT_NOFILE and probes the real descriptor ceiling (it opens descriptors until the
 * kernel refuses, for a moment). Call it before any listener thread exists; the data layer's start then
 * reuses the answer instead of starving the other threads of descriptors at boot. Idempotent. */
void ava1_fd_limits_probe(void);
uint32_t ava1_pend_share(void);
/* Waits until a pending-fd slot is free, then takes it (before the open). Gives up (0) when
 * `stop` is set; `idle` runs between polls (the apply engine runs queued sync work there).
 * 1 = slot taken. */
int ava1_pend_reserve(int (*stopping)(void *), void (*idle)(void *), void *arg);
void ava1_pend_release(uint32_t n);
int ava1_pend_full(void);  /* the global pending-fd count has reached its share */
/* Tests only: 0 = derive from the limit; else forces the budget. Peaks are high-water marks
 * since the last reset. */
extern uint32_t ava1_data_test_fd_budget;
/* Large-file descriptors (a part file, and an outboard from two groups up, per open file): all jobs
 * share a quarter of the budget, one job half of that. try_reserve takes `n` or returns 0. */
uint32_t ava1_lf_share(void);
uint32_t ava1_lf_job_share(void);
int ava1_lf_try_reserve(uint32_t n);
void ava1_lf_release(uint32_t n);
void ava1_lf_force_reserve(uint32_t n);
uint32_t ava1_lf_peak(void);
void ava1_lf_peak_reset(void);
/* fsync of every job's journal and pack segments, for the exit watchdog (see ava1_exit_flush). Takes no
 * job lock it cannot get at once. */
void ava1_data_flush_for_exit(void);
uint32_t ava1_pend_in_use(void);
uint32_t ava1_pend_peak(void);
void ava1_pend_peak_reset(void);
extern uint32_t ava1_data_test_cal_peak; /* most fds disk.calibrate held at once */
/* Writes a failure's cause into an RPC reply body (sent with the error status). */
void ava1_rpc_msg(uint8_t *out, size_t cap, size_t *out_len, const char *fmt, ...);
/* The truncation-safe way for a ported management handler to produce a text reply (SPEC.md §7.3):
 * formats into `out`, sets *out_len and returns AVA1_STATUS_OK, or, when the text does not fit `cap`
 * (snprintf returned >= cap) or cannot be formatted, returns AVA1_ERR_INTERNAL with the cause
 * "reply truncated" in `out`. A reply is never `ok` with a clipped body. */
int ava1_rpc_text(uint8_t *out, size_t cap, size_t *out_len, const char *fmt, ...);

/* Runs fn(arg) on a short-lived detached thread that ava1_data_stop waits for. 0 or -1. */
int ava1_data_spawn(void *(*fn)(void *), void *arg);

/* The server's data hooks (SPEC.md §11-§12): JobOpen and the job conversation on the
 * control connection, lane frames admitted against credit, Received before any disk work,
 * a closed session parking its jobs. Every hook only decodes, routes and queues: JobOpen's
 * work runs on a short-lived thread, a receiver job's frames on its feeder thread. */
const ava1_data_hooks_t *ava1_data_hooks(void);
/* The data plane's RPC methods (SPEC.md §13.5): job.copy, job.status, job.cancel and
 * disk.calibrate. Returns AVA1_STATUS_OK or an AVA1_ERR_*; -1 when the method is not the
 * data layer's, so an embedder's own handler runs (ava1_glue.c, Task 21). */
int ava1_data_rpc(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap,
                  size_t *out_len);
/* Five file-create/fsync measurements used to choose an initial worker count. */
int ava1_calibrate(const uint8_t *body, uint32_t len, uint8_t *out, size_t cap, size_t *out_len);
/* Attaches a job to session `sid`: its messages go there (waiting sends), its lanes are
 * counted, frames held for an earlier session are dropped and lane frames wait for the
 * new session's map. `credit` nonzero is this attach's grant (JobOpenAck.credit): the
 * sender's allowance restarts from it; 0 (Resume) keeps the allowance running. Returns 0,
 * or -1 when the job is no longer listed or its feeder cannot start (the job is parked). */
int ava1_job_attach(ava1_job_t *j, const uint8_t sid[16], uint64_t credit);

/* Records a failure found off the job thread (a sender job's on_frame hook, a worker):
 * the job thread ends the job within one 25 ms tick (ruling C4). Never blocks on I/O. */
void ava1_data_fail_soon(ava1_job_t *j, uint16_t status, const char *what);

/* Tests only (0 in the payload): JobOpen's work waits this long before it starts, and an
 * OK map this long before it is sent. */
extern uint32_t ava1_data_test_open_delay_ms;
extern uint32_t ava1_data_test_open_work_delay_ms; /* the slow work of a JobOpen waits this long */
extern uint32_t ava1_data_test_map_delay_ms;
extern int ava1_data_test_ack_fail;        /* JobOpenAck's send "fails" with this code */
extern int ava1_data_test_feeder_fail;     /* a job's feeder thread "cannot start" */
extern uint32_t ava1_data_test_feed_delay_ms; /* the feeder waits this long after taking lane frames */
extern int ava1_data_test_reserve_fail;    /* ava1_apply_reserve "fails" for every lane frame */
extern int ava1_data_test_lane_alloc_fail; /* the lane frame's calloc "fails" */
extern int ava1_data_test_fb_force;   /* every Received goes through the waiting-send fallback */
/* Tests only: sets ava1_data_test_open_work_delay_ms (the ctest suite's FFI). */
void ava1_test_set_open_work_delay_ms(uint32_t ms);

#endif
