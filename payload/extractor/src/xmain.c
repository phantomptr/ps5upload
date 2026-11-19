/* ps5upload-extractor's entry point: read the request, extract, report, exit.
 *
 * On the PS5 the paths are fixed (the helper's extract folder, XMAIN_DIR); the host build takes them
 * as arguments so the tests can run several extractions side by side:
 *   ps5upload-extractor-host <request.json> <status.json> <cancel-file>
 *
 * The status is rewritten at most every XMAIN_STATUS_MS while running (and at the end), the cancel
 * file looked for at most every XMAIN_CANCEL_MS. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "xjob.h"
#include "xreq.h"

#ifdef __PROSPERO__
#include "proc_identity.h"
#endif

#define XMAIN_DIR "/data/ps5upload/extract"
#define XMAIN_STATUS_MS 250
#define XMAIN_CANCEL_MS 150

typedef struct {
    const char *status_path;
    const char *cancel_path;
    long long last_status, last_cancel;
    int cancel_seen;
} xmain_t;

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int cancelled(xjob_t *j) {
    xmain_t *m = j->ctx;
    long long t;
    struct stat st;
    if (m->cancel_seen) return 1;
    t = now_ms();
    if (t - m->last_cancel < XMAIN_CANCEL_MS) return 0;
    m->last_cancel = t;
    if (stat(m->cancel_path, &st) == 0) m->cancel_seen = 1;
    return m->cancel_seen;
}

static void progress(xjob_t *j, int force) {
    xmain_t *m = j->ctx;
    long long t = now_ms();
    if (!force && t - m->last_status < XMAIN_STATUS_MS) return;
    m->last_status = t;
    (void)xstatus_write(m->status_path, j, "running", (long)getpid());
}

int main(int argc, char **argv) {
    static xjob_t job; /* ~3 KiB: off the stack */
    xmain_t m;
    const char *req_path;
    int rc;
#ifdef __PROSPERO__
    /* Not "payload.elf" (the name elfldr gives raw payloads), which other payloads sweep for, and
     * not "ps5upload-*" either: the helper SIGKILLs every process of that name as a stray instance of
     * itself (runtime_sweep_our_instances), which ended the first hardware run mid-archive. */
    proc_name_set_self("ps5u-extract");
    (void)argc;
    (void)argv;
    req_path = XMAIN_DIR "/request.json";
    m.status_path = XMAIN_DIR "/status.json";
    m.cancel_path = XMAIN_DIR "/cancel";
    {
        /* The loader hands us its socket as stdio; the helper has long closed it. */
        int devnull = open("/dev/null", O_RDONLY), log = open(XMAIN_DIR "/extractor.log", O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (devnull >= 0) {
            dup2(devnull, 0);
            if (devnull != 0) close(devnull);
        }
        if (log >= 0) {
            dup2(log, 1);
            dup2(log, 2);
            if (log > 2) close(log);
        }
        setvbuf(stdout, NULL, _IONBF, 0);
        setvbuf(stderr, NULL, _IONBF, 0);
    }
#else
    if (argc != 4) {
        fprintf(stderr, "usage: %s <request.json> <status.json> <cancel-file>\n", argv[0]);
        return 2;
    }
    req_path = argv[1];
    m.status_path = argv[2];
    m.cancel_path = argv[3];
#endif
    signal(SIGPIPE, SIG_IGN);
    m.last_status = m.last_cancel = 0;
    m.cancel_seen = 0;
    memset(&job, 0, sizeof job);
    job.ctx = &m;
    job.cancelled = cancelled;
    job.progress = progress;
    if (xreq_read_file(req_path, &job) != 0) {
        unlink(req_path);
        xj_fail(&job, "fs_extract_bad_request");
        xstatus_write(m.status_path, &job, "failed", (long)getpid());
        return 1;
    }
    unlink(req_path);
#ifndef __PROSPERO__
    {
        /* Tests: a drive with this much room, writes that fail past this many bytes, a slow drive. */
        const char *f = getenv("XTEST_FREE"), *w = getenv("XTEST_WRITE_LIMIT"), *d = getenv("XTEST_WRITE_DELAY_US");
        if (f) job.test_free = strtoull(f, NULL, 10);
        if (w) job.test_write_limit = strtoull(w, NULL, 10);
        if (d) job.test_write_delay_us = (unsigned)strtoul(d, NULL, 10);
    }
#endif
    fprintf(stderr, "extract %s -> %s (merge %d, replace %d, password %s)\n", job.archive, job.dest, job.merge, job.replace,
            job.has_password ? "yes" : "no");
    /* The helper knows this extractor only by the status it reads here. Without a first one (the
     * drive holding the status folder is full, say) it gives up after 20 s as "not started" and
     * removes the destination folder: under an extractor still writing into it, which nothing
     * follows and no Stop reaches, while a second extraction may be started beside it. So no
     * status, no extraction. */
    m.last_status = now_ms();
    if (xstatus_write(m.status_path, &job, "running", (long)getpid()) != 0) {
        fprintf(stderr, "cannot write the status (%s): not extracting\n", strerror(errno));
        memset(job.password, 0, sizeof job.password);
        return 1;
    }
    rc = xj_run(&job);
    memset(job.password, 0, sizeof job.password);
    if (rc != 0 && !job.cause[0]) xj_fail(&job, "fs_extract_corrupt");
    fprintf(stderr, "%s: %llu files, %llu folders, %llu bytes, %llu skipped, %llu unsafe, %llu links%s%s\n",
            rc == 0 ? "done" : "failed", (unsigned long long)job.files, (unsigned long long)job.dirs,
            (unsigned long long)job.bytes, (unsigned long long)job.skipped, (unsigned long long)job.unsafe,
            (unsigned long long)job.links, job.cause[0] ? ": " : "", job.cause);
    job.current[0] = 0;
    xstatus_write(m.status_path, &job, rc == 0 ? "done" : "failed", (long)getpid());
    return rc == 0 ? 0 : 1;
}
