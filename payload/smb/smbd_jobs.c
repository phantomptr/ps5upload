/* See smbd_jobs.h. */
#include "smbd_jobs.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "smbd_library.h"
#include "smbd_log.h"
#include "smbd_proto.h"

#define JOB_STACK (512u * 1024u)
/* Room a copy leaves on the console's disk beyond what it writes. */
#define JOB_SPACE_MARGIN (64ull * 1024ull * 1024ull)

typedef enum { ST_FREE = 0, ST_RUNNING, ST_DONE, ST_FAILED, ST_CANCELLED } job_state_t;

typedef struct {
    job_state_t state;
    job_kind_t kind;
    char id[33];
    smbconn_t conn;
    char src[SMBP_PATH_MAX];
    char dst[SMBP_PATH_MAX];
    int overwrite;
    int err;
    char failed[SMBP_PATH_MAX];
    char detail[160];
    unsigned dialect;
    time_t started;
    time_t ended;
    copy_progress_t pr;
} job_t;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static job_t g_jobs[SMBD_JOBS_MAX];

static const char *kind_name(job_kind_t k) {
    return k == JOB_DOWN ? "download" : k == JOB_UP ? "upload" : k == JOB_REMOVE ? "remove" : "scan";
}

static const char *state_name(job_state_t s) {
    switch (s) {
    case ST_RUNNING: return "running";
    case ST_DONE: return "done";
    case ST_CANCELLED: return "cancelled";
    default: return "failed";
    }
}

static void *job_main(void *arg) {
    job_t *j = arg;
    xfs_t smb, local;
    int rc;
    smbd_name_thread();
    localfs_init(&local);
    memset(&smb, 0, sizeof smb);
    if (j->kind == JOB_SCAN_LOCAL) {
        rc = lib_scan(&local, j->dst, SMBD_LIB_CONSOLE_ROOTS, SMBD_LIB_CONSOLE_SKIP, SMBD_LIB_DIR, NULL, &j->pr);
    } else if ((rc = smbfs_open(&smb, &j->conn, j->detail, sizeof j->detail)) == 0) {
        copy_opts_t o;
        memset(&o, 0, sizeof o);
        j->dialect = smbfs_dialect(&smb);
        o.overwrite = j->overwrite;
        if (j->kind == JOB_DOWN) {
            o.skip_mac_metadata = 1;
            o.space_margin = JOB_SPACE_MARGIN;
            rc = copy_run(&smb, j->src, &local, j->dst, &o, &j->pr, j->failed, sizeof j->failed);
        } else if (j->kind == JOB_UP) {
            rc = copy_run(&local, j->src, &smb, j->dst, &o, &j->pr, j->failed, sizeof j->failed);
        } else if (j->kind == JOB_SCAN_SMB) {
            xstat_t st;
            /* A start folder that is not there is reported (not_found), not skipped into an
             * empty "complete" result; the share's top ("") needs no check. */
            if (j->src[0] && (rc = smb.ops->stat(&smb, j->src, &st)) == 0 && st.kind != XK_DIR) rc = XE_NOT_DIR;
            if (rc == 0) rc = lib_scan(&smb, j->dst, j->src, NULL, SMBD_LIB_DIR, NULL, &j->pr);
        } else {
            rc = copy_remove_tree(&smb, j->src, &j->pr);
        }
        smbfs_close(&smb);
    }
    smbconn_clear(&j->conn);
    pthread_mutex_lock(&g_mu);
    j->err = rc;
    j->state = rc == 0 ? ST_DONE : (rc == XE_CANCELLED || j->pr.cancel) ? ST_CANCELLED : ST_FAILED;
    j->ended = time(NULL);
    pthread_mutex_unlock(&g_mu);
    smbd_log("job %s %s %s: %s%s%s (%llu bytes, %u files, %u reconnects)", j->id, kind_name(j->kind),
             state_name(j->state), rc ? smbp_err_token(rc) : "ok", j->failed[0] ? " at " : "", j->failed,
             (unsigned long long)j->pr.bytes_done, (unsigned)j->pr.files_done, (unsigned)j->pr.reconnects);
    return NULL;
}

static int running_locked(void) {
    int i, n = 0;
    for (i = 0; i < SMBD_JOBS_MAX; i++)
        if (g_jobs[i].state == ST_RUNNING) n++;
    return n;
}

int jobs_running(void) {
    int n;
    pthread_mutex_lock(&g_mu);
    n = running_locked();
    pthread_mutex_unlock(&g_mu);
    return n;
}

/* A free slot, else the oldest finished one (the page reads a job's end within seconds). */
static job_t *slot_locked(void) {
    job_t *best = NULL;
    int i;
    time_t now = time(NULL);
    for (i = 0; i < SMBD_JOBS_MAX; i++) {
        job_t *j = &g_jobs[i];
        if (j->state == ST_FREE) return j;
        if (j->state != ST_RUNNING && now - j->ended >= SMBD_JOB_KEEP_S) return j;
        if (j->state != ST_RUNNING && (!best || j->ended < best->ended)) best = j;
    }
    return best;
}

int jobs_start(job_kind_t kind, const smbconn_t *c, const char *src, const char *dst, int overwrite, char id[33]) {
    pthread_attr_t at;
    pthread_t th;
    job_t *j;
    uint8_t raw[16];
    int i, rc;
    pthread_mutex_lock(&g_mu);
    if (running_locked() >= SMBD_JOBS_RUNNING_MAX || !(j = slot_locked())) {
        pthread_mutex_unlock(&g_mu);
        return XE_BUSY;
    }
    if (kind == JOB_SCAN_SMB || kind == JOB_SCAN_LOCAL) {
        for (i = 0; i < SMBD_JOBS_MAX; i++) {
            const job_t *o = &g_jobs[i];
            if (o->state == ST_RUNNING && (o->kind == JOB_SCAN_SMB || o->kind == JOB_SCAN_LOCAL) && strcmp(o->dst, dst) == 0) {
                pthread_mutex_unlock(&g_mu);
                return XE_BUSY;
            }
        }
    }
    memset(j, 0, sizeof *j);
    arc4random_buf(raw, sizeof raw);
    for (i = 0; i < 16; i++) snprintf(j->id + 2 * i, 3, "%02x", raw[i]);
    j->kind = kind;
    j->conn = *c;
    j->conn.timeout_s = 15;
    snprintf(j->src, sizeof j->src, "%s", src);
    snprintf(j->dst, sizeof j->dst, "%s", dst ? dst : "");
    j->overwrite = overwrite;
    j->started = time(NULL);
    j->state = ST_RUNNING;
    memcpy(id, j->id, 33);
    pthread_attr_init(&at);
    pthread_attr_setstacksize(&at, JOB_STACK);
    pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
    rc = pthread_create(&th, &at, job_main, j);
    pthread_attr_destroy(&at);
    if (rc != 0) {
        smbconn_clear(&j->conn);
        memset(j, 0, sizeof *j);
        pthread_mutex_unlock(&g_mu);
        return XE_NOMEM;
    }
    pthread_mutex_unlock(&g_mu);
    smbd_log("job %s %s started", id, kind_name(kind));
    return 0;
}

static job_t *find_locked(const char *id) {
    int i;
    for (i = 0; i < SMBD_JOBS_MAX; i++)
        if (g_jobs[i].state != ST_FREE && strcmp(g_jobs[i].id, id) == 0) return &g_jobs[i];
    return NULL;
}

static void status_locked(const job_t *j, sj_buf_t *b) {
    char cur[sizeof j->pr.current];
    memcpy(cur, j->pr.current, sizeof cur);
    cur[sizeof cur - 1] = '\0';
    sj_raw(b, "{");
    sj_kv_str(b, 0, "job", j->id);
    sj_kv_str(b, 1, "kind", kind_name(j->kind));
    sj_kv_str(b, 1, "state", state_name(j->state));
    sj_fmt(b, ",\"bytes_done\":%llu,\"bytes_total\":%llu,\"files_done\":%u,\"files_total\":%u,\"scanning\":%s",
           (unsigned long long)j->pr.bytes_done, (unsigned long long)j->pr.bytes_total, (unsigned)j->pr.files_done,
           (unsigned)j->pr.files_total, j->pr.scanning ? "true" : "false");
    sj_fmt(b, ",\"reconnects\":%u,\"dialect\":%u,\"started\":%lld,\"ended\":%lld", (unsigned)j->pr.reconnects, j->dialect,
           (long long)j->started, (long long)(j->state == ST_RUNNING ? 0 : j->ended));
    sj_kv_str(b, 1, "current", cur);
    sj_kv_str(b, 1, "src", j->src);
    sj_kv_str(b, 1, "dest", j->dst);
    if (j->state == ST_RUNNING || j->state == ST_DONE) {
        sj_raw(b, ",\"error\":null");
    } else {
        sj_kv_str(b, 1, "error", smbp_err_token(j->err ? j->err : XE_CANCELLED));
    }
    sj_kv_str(b, 1, "failed", j->failed);
    sj_kv_str(b, 1, "detail", j->detail);
    sj_raw(b, "}");
}

int jobs_status(const char *id, sj_buf_t *b) {
    job_t *j;
    pthread_mutex_lock(&g_mu);
    j = find_locked(id);
    if (j) status_locked(j, b);
    pthread_mutex_unlock(&g_mu);
    return j ? 0 : XE_NOT_FOUND;
}

void jobs_list(sj_buf_t *b) {
    int i, first = 1;
    pthread_mutex_lock(&g_mu);
    sj_raw(b, "{\"jobs\":[");
    for (i = 0; i < SMBD_JOBS_MAX; i++) {
        if (g_jobs[i].state == ST_FREE) continue;
        if (!first) sj_raw(b, ",");
        first = 0;
        status_locked(&g_jobs[i], b);
    }
    sj_raw(b, "]}");
    pthread_mutex_unlock(&g_mu);
}

int jobs_cancel(const char *id) {
    job_t *j;
    pthread_mutex_lock(&g_mu);
    j = find_locked(id);
    if (j && j->state == ST_RUNNING) j->pr.cancel = 1;
    pthread_mutex_unlock(&g_mu);
    return j ? 0 : XE_NOT_FOUND;
}

void jobs_cancel_all(void) {
    int i;
    pthread_mutex_lock(&g_mu);
    for (i = 0; i < SMBD_JOBS_MAX; i++)
        if (g_jobs[i].state == ST_RUNNING) g_jobs[i].pr.cancel = 1;
    pthread_mutex_unlock(&g_mu);
}
