#include "jobs.h"

#include <string.h>
#include <stdio.h>
#include <stdint.h>

const char *inst_job_phase_str(inst_job_phase_t phase) {
    switch (phase) {
        case INST_JOB_SERVING:  return "serving";
        case INST_JOB_ACCEPTED: return "accepted";
        case INST_JOB_DONE:     return "done";
        case INST_JOB_FAILED:   return "failed";
        case INST_JOB_NONE:
        default:                return "none";
    }
}

int inst_admit_install(inst_job_phase_t active_phase, int active_complete) {
    /* A serving job blocks only while Sony may still need its file. Once every
     * byte has been served the caller retires it and admits the new install;
     * otherwise a fully-read job blocked for its whole idle-retire window. */
    return (active_phase == INST_JOB_SERVING && !active_complete) ? 0 : 1;
}

void inst_coverage_reset(inst_coverage_t *c) {
    c->count = 0;
}

static void coverage_coalesce_smallest_gap(inst_coverage_t *c) {
    /* Intervals are kept sorted by start (see add). Merge the adjacent pair
     * with the smallest gap so the array never exceeds INST_COVERAGE_MAX. */
    size_t best = 0;
    uint64_t best_gap = UINT64_MAX;
    for (size_t i = 0; i + 1 < c->count; i++) {
        uint64_t gap = c->iv[i + 1].start - c->iv[i].end - 1;
        if (gap < best_gap) { best_gap = gap; best = i; }
    }
    if (c->iv[best + 1].end > c->iv[best].end)
        c->iv[best].end = c->iv[best + 1].end;
    for (size_t i = best + 1; i + 1 < c->count; i++)
        c->iv[i] = c->iv[i + 1];
    c->count--;
}

void inst_coverage_add(inst_coverage_t *c, uint64_t start, uint64_t end) {
    if (end < start) return;
    /* insert sorted by start */
    size_t pos = 0;
    while (pos < c->count && c->iv[pos].start < start) pos++;
    if (c->count >= INST_COVERAGE_MAX) {
        /* make room, then re-derive; coalescing keeps the union monotone */
        coverage_coalesce_smallest_gap(c);
        if (pos > c->count) pos = c->count;
    }
    for (size_t i = c->count; i > pos; i--) c->iv[i] = c->iv[i - 1];
    c->iv[pos].start = start;
    c->iv[pos].end = end;
    c->count++;
    /* merge overlaps/adjacency in a single forward pass */
    size_t w = 0;
    for (size_t r = 1; r < c->count; r++) {
        if (c->iv[r].start <= c->iv[w].end + 1) {
            if (c->iv[r].end > c->iv[w].end) c->iv[w].end = c->iv[r].end;
        } else {
            w++;
            c->iv[w] = c->iv[r];
        }
    }
    c->count = w + 1;
}

uint64_t inst_coverage_bytes(const inst_coverage_t *c) {
    uint64_t sum = 0;
    for (size_t i = 0; i < c->count; i++)
        sum += c->iv[i].end - c->iv[i].start + 1;
    return sum;
}

int inst_coverage_complete(const inst_coverage_t *c, uint64_t total) {
    if (total == 0) return 1;
    return c->count == 1 && c->iv[0].start == 0 && c->iv[0].end == total - 1;
}

int inst_job_should_finish(int covered_complete, double idle_seconds) {
    return (covered_complete && idle_seconds >= 600.0) ? 1 : 0;
}

void inst_ring_reset(inst_job_ring_t *r) {
    r->next = 0;
    r->count = 0;
    for (size_t i = 0; i < INST_JOB_RING; i++) r->id[i][0] = '\0';
}

static int ring_find(const inst_job_ring_t *r, const char *id) {
    for (size_t i = 0; i < INST_JOB_RING; i++)
        if (r->id[i][0] != '\0' && strcmp(r->id[i], id) == 0) return (int)i;
    return -1;
}

size_t inst_ring_put(inst_job_ring_t *r, const char *id, inst_job_phase_t phase,
                     uint32_t code, uint64_t bytes_served, uint64_t total) {
    int slot = ring_find(r, id);
    size_t i;
    if (slot >= 0) {
        i = (size_t)slot;
    } else {
        i = r->next;
        r->next = (r->next + 1) % INST_JOB_RING;
        if (r->count < INST_JOB_RING) r->count++;
        snprintf(r->id[i], INST_JOBID_MAX, "%s", id);
    }
    r->phase[i] = phase;
    r->code[i] = code;
    r->bytes_served[i] = bytes_served;
    r->total[i] = total;
    return i;
}

int inst_ring_get(const inst_job_ring_t *r, const char *id,
                  inst_job_phase_t *phase, uint32_t *code,
                  uint64_t *bytes_served, uint64_t *total) {
    int slot = ring_find(r, id);
    if (slot < 0) return 0;
    if (phase) *phase = r->phase[slot];
    if (code) *code = r->code[slot];
    if (bytes_served) *bytes_served = r->bytes_served[slot];
    if (total) *total = r->total[slot];
    return 1;
}
