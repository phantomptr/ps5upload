/* JOB_OP_APP_DUMP: the job (see include/app_dump.h). The dump is app_dump_core.c; this checks the
 * destination against the writable roots, runs one dump at a time and reports to the AVA1 job.
 *
 * Copyright (C) 2026 PhantomPtr. Part of a port of ps5-app-dumper (EchoStretch, slopmaster33),
 * GPLv3-or-later; see app_dump_core.c. */
#include "app_dump.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "fs_jobs.h"
#include "path_policy.h"
#include "runtime.h"

static pthread_mutex_t g_busy = PTHREAD_MUTEX_INITIALIZER;
/* Set while g_busy is held by a dump: read by app_dump_running without touching the lock (a trylock
 * there would make a dump that starts at that moment fail with dump_busy). */
static volatile int g_dump_running = 0;

int app_dump_running(void) { return __atomic_load_n(&g_dump_running, __ATOMIC_ACQUIRE); }

static int hk_cancelled(void *a) { return ava1_op_cancelled((ava1_op_ctx_t *)a); }
static void hk_total(void *a, uint64_t files, uint64_t bytes) { ava1_op_set_total((ava1_op_ctx_t *)a, files, bytes); }
static void hk_add(void *a, uint64_t files, uint64_t bytes) { ava1_op_add((ava1_op_ctx_t *)a, files, bytes); }
static void hk_note(void *a, const char *what) { ava1_op_message((ava1_op_ctx_t *)a, "%s", what); }

int app_dump_op(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    app_dump_args_t a;
    app_dump_result_t *r;
    app_dump_hooks_t h = {c, hk_cancelled, hk_total, hk_add, hk_note};
    const char *bad;
    int rc, status;
    (void)arg;
    if ((bad = app_dump_parse_args((const char *)args, n, &a)) != NULL) {
        ava1_op_message(c, "%s", bad);
        return AVA1_ERR_PROTOCOL;
    }
    /* Where it may write: the writable roots, never the shadow mounts. */
    if (fsj_normalize_path(a.dest) != 0 || !is_path_allowed(a.dest) || path_tree_op_refused(a.dest) ||
        strncmp(a.dest, "/mnt/shadowmnt/", 15) == 0 || strncmp(a.dest, "/mnt/sandbox/", 13) == 0) {
        ava1_op_message(c, "dump_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    if (pthread_mutex_trylock(&g_busy) != 0) {
        ava1_op_message(c, "dump_busy");
        return AVA1_ERR_BUSY;
    }
    __atomic_store_n(&g_dump_running, 1, __ATOMIC_RELEASE);
    if (!(r = calloc(1, sizeof *r))) {
        __atomic_store_n(&g_dump_running, 0, __ATOMIC_RELEASE);
        pthread_mutex_unlock(&g_busy);
        ava1_op_message(c, "dump_io_failed: out of memory");
        return AVA1_ERR_IO;
    }
    fprintf(stderr, "app_dump: %s -> %s (decrypt %d, fself %d, dlc %d)\n", a.title_id, a.dest, a.decrypt, a.fself,
            a.dlc);
    rc = app_dump_run(&a, &h, r);
    switch (rc) {
    case APP_DUMP_OK: {
        char *body = malloc(16384);
        int len = body ? app_dump_result_json(r, a.title_id, 0, 0, body, 16384) : -1;
        if (len < 0 || ava1_op_set_result(c, body, (size_t)len) != 0) {
            ava1_op_message(c, "dump_io_failed: result too large");
            status = AVA1_ERR_IO;
        } else {
            /* Done; a firmware without decryption says so (the result says it too). */
            ava1_op_message(c, "%s", r->decrypt_skipped);
            status = AVA1_STATUS_OK;
        }
        free(body);
        fprintf(stderr, "app_dump: %s done: %llu files, %llu bytes, %llu decrypted, %llu failed\n", r->path,
                (unsigned long long)r->files, (unsigned long long)r->bytes, (unsigned long long)r->decrypted,
                (unsigned long long)r->decrypt_failed);
        break;
    }
    case APP_DUMP_E_NOT_RUNNING: status = AVA1_ERR_IO; break;
    case APP_DUMP_E_EXISTS: status = AVA1_ERR_EXISTS; break;
    case APP_DUMP_E_NO_SPACE: status = AVA1_ERR_NO_SPACE; break;
    case APP_DUMP_E_CANCELLED: status = AVA1_ERR_CANCELLED; break;
    default: status = AVA1_ERR_IO; break;
    }
    if (status != AVA1_STATUS_OK && rc != APP_DUMP_OK) {
        ava1_op_message(c, "%s", r->cause[0] ? r->cause : "dump_io_failed");
        fprintf(stderr, "app_dump: %s failed: %s\n", a.title_id, r->cause);
    }
    free(r);
    __atomic_store_n(&g_dump_running, 0, __ATOMIC_RELEASE);
    pthread_mutex_unlock(&g_busy);
    return status;
}

/* Not repeatable: a repeat after a lost reply answers from the job instead of finding the folder it
 * wrote and failing with dump_exists. */
void app_dump_register(void) { (void)ava1_op_register_ex(AVA1_JOB_OP_APP_DUMP, app_dump_op, NULL, AVA1_OP_ONCE); }
