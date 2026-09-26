#include "jobs.h"

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

int inst_admit_install(inst_job_phase_t active_phase) {
    return active_phase == INST_JOB_SERVING ? 0 : 1;
}
