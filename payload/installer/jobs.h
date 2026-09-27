#ifndef PS5UPLOAD_INSTALLER_JOBS_H
#define PS5UPLOAD_INSTALLER_JOBS_H

#include <stddef.h>
#include <stdint.h>

#include "protocol.h"  /* INST_JOBID_MAX */

typedef enum {
    INST_JOB_NONE = 0,
    INST_JOB_SERVING,
    INST_JOB_ACCEPTED,
    INST_JOB_DONE,
    INST_JOB_FAILED,
} inst_job_phase_t;

/* "serving" | "accepted" | "done" | "failed" | "none". */
const char *inst_job_phase_str(inst_job_phase_t phase);

/* Admission for a new install given the active job's phase and whether its
 * loopback file has been fully served. Returns 1 to admit, 0 to reject as
 * busy. Only a SERVING loopback job that Sony has not finished reading
 * blocks; an ACCEPTED url job never does (its bytes are the engine
 * pkg-host's). A fully-read serving job is retired by the caller. */
int inst_admit_install(inst_job_phase_t active_phase, int active_complete);

#define INST_COVERAGE_MAX 256

typedef struct { uint64_t start; uint64_t end; } inst_interval_t; /* inclusive */

typedef struct {
    inst_interval_t iv[INST_COVERAGE_MAX];
    size_t count;
} inst_coverage_t;

void inst_coverage_reset(inst_coverage_t *c);
/* Add inclusive [start,end], merging overlaps and adjacency. Bounded: if the
 * set is full and the new interval is disjoint, the pair of neighbours with
 * the smallest gap is coalesced first (this only ever over-reports coverage
 * for the gap bytes, never the served bytes — and it keeps the array bounded
 * on a console). */
void inst_coverage_add(inst_coverage_t *c, uint64_t start, uint64_t end);
uint64_t inst_coverage_bytes(const inst_coverage_t *c);
int inst_coverage_complete(const inst_coverage_t *c, uint64_t total);

/* serving -> done when coverage is complete AND idle >= 600s. */
int inst_job_should_finish(int covered_complete, double idle_seconds);

#define INST_JOB_RING 16

typedef struct {
    char             id[INST_JOB_RING][INST_JOBID_MAX];
    inst_job_phase_t phase[INST_JOB_RING];
    uint32_t         code[INST_JOB_RING];
    uint64_t         bytes_served[INST_JOB_RING];
    uint64_t         total[INST_JOB_RING];
    size_t           next;   /* write cursor (mod INST_JOB_RING) */
    size_t           count;  /* live entries, up to INST_JOB_RING */
} inst_job_ring_t;

void inst_ring_reset(inst_job_ring_t *r);
/* Insert or update by id. Updates in place if the id exists; else writes at
 * the cursor, evicting the oldest once full. Returns the slot index. */
size_t inst_ring_put(inst_job_ring_t *r, const char *id, inst_job_phase_t phase,
                     uint32_t code, uint64_t bytes_served, uint64_t total);
/* 1 + fills outs if found, else 0. */
int inst_ring_get(const inst_job_ring_t *r, const char *id,
                  inst_job_phase_t *phase, uint32_t *code,
                  uint64_t *bytes_served, uint64_t *total);

#endif /* PS5UPLOAD_INSTALLER_JOBS_H */
