#ifndef PS5UPLOAD_INSTALLER_JOBS_H
#define PS5UPLOAD_INSTALLER_JOBS_H

#include <stddef.h>
#include <stdint.h>

typedef enum {
    INST_JOB_NONE = 0,
    INST_JOB_SERVING,
    INST_JOB_ACCEPTED,
    INST_JOB_DONE,
    INST_JOB_FAILED,
} inst_job_phase_t;

/* "serving" | "accepted" | "done" | "failed" | "none". */
const char *inst_job_phase_str(inst_job_phase_t phase);

/* Admission for a new install given the active job's phase. Returns 1 to
 * admit, 0 to reject as busy. Only a SERVING loopback job blocks; an
 * ACCEPTED url job never does (its bytes are the engine pkg-host's). */
int inst_admit_install(inst_job_phase_t active_phase);

#endif /* PS5UPLOAD_INSTALLER_JOBS_H */
