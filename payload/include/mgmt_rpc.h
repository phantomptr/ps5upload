/* AVA1 management dispatcher (P3 Task 2).
 *
 * Routes an AVA1 management RPC (methods 4..141, SPEC.md §7.3) to the handler in
 * runtime.c that already implements it. The handler is called unchanged with client_fd = -1;
 * the frame it would have written to the socket is captured by a thread-local sink
 * (runtime.c's send_frame asks mgmt_capture_active() first), and this file turns the
 * captured frame into an RpcResponse: an ERROR frame or a legacy {"ok":false,...} body
 * becomes an AVA1 error status with the legacy token as its cause; anything that does not
 * fit the reply is ERR_INTERNAL "reply truncated", never a clipped OK.
 *
 * This file does not include runtime.c's types, so the host test harness (ava1-ctest)
 * compiles it as is, with stub handlers. The real table is payload/src/mgmt_table.def,
 * installed by runtime_mgmt_install() (runtime.c).
 */
#ifndef PS5UPLOAD_MGMT_RPC_H
#define PS5UPLOAD_MGMT_RPC_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_gen.h" /* AVA1_METHOD_* for the table */
#include "ava1_op.h"  /* job.run operations (AVA1_JOB_OP_*) */

/* Entry flags. */
#define MGMT_SONY (1u << 0) /* the handler reaches Sony code (register/profile/registry/remoteplay/notif or a Sony API) and takes the serialisation lock itself; the dispatcher adds no lock and no Sony call */
#define MGMT_LONG (1u << 1) /* refused here: the operation runs as a job (job.run) */

/* The management worker stack (the old management thread had 512 KiB; AVA1 workers 256 KiB). */
#define MGMT_THREAD_STACK (512u * 1024u)

/* The frame number of an error reply: what a handler passes to mgmt_reply() to fail (any other number is a
 * success). */
#define MGMT_FRAME_ERROR 3u

/* The cause a handler returns when its answer did not fit (SPEC.md §7.3). */
#define MGMT_ERR_TRUNCATED "reply truncated"
/* The longest error cause (a kept failure body longer than this travels as its token only). */
#define MGMT_CAUSE_MAX 200u

struct mgmt_ctx;

/* A legacy handler adapted to one signature: (runtime state, fd, trace id, request
 * body as a NUL-terminated string, its length). It answers through send_frame(). */
typedef int (*mgmt_legacy_fn)(void *state, int fd, uint64_t trace_id, const char *body, uint64_t body_len);

/* One table entry's runner: decodes the AVA1 request, calls the legacy handler through
 * mgmt_legacy_call() and encodes the reply into cx->out. Returns an AVA1 status (0 = OK);
 * on an error the cause is in cx->out / cx->out_len. */
typedef int (*mgmt_run_fn)(const uint8_t *req, uint32_t req_len, struct mgmt_ctx *cx);

typedef struct {
    uint16_t method;       /* AVA1_METHOD_* */
    uint16_t legacy_frame; /* the legacy frame number: g_inflight_frame_type and the crash breadcrumb */
    uint16_t ack_frame;    /* the legacy success frame (informational; the sink accepts any non-error frame) */
    uint32_t flags;        /* MGMT_* */
    mgmt_run_fn run;
} mgmt_entry_t;

typedef struct mgmt_ctx {
    void *state;            /* runtime_state_t * */
    uint8_t *out;           /* the RPC reply buffer (RPC_OUT_MAX) */
    size_t cap;
    size_t out_len;
    const mgmt_entry_t *entry;
} mgmt_ctx_t;

/* The frame a legacy handler produced. */
typedef struct {
    uint8_t *body; /* heap, NUL-terminated; mgmt_reply_free() */
    size_t len;
} mgmt_reply_t;

void mgmt_reply_free(mgmt_reply_t *r);

/* Installs the table (not copied; it must outlive the server) and the per-call environment:
 * `enter` runs on the worker before every handler (credential elevation, the in-flight
 * frame marker) and `leave` after it. Either may be NULL. Returns 0, or -1 on a duplicate
 * method, an entry without a runner, or a NULL table. */
int mgmt_rpc_install(const mgmt_entry_t *table, size_t n, void *state, void (*enter)(uint16_t legacy_frame),
                     void (*leave)(void));
/* 1 when a table is installed (the server then advertises CAP_MGMT). */
int mgmt_rpc_installed(void);

/* Runs a management method. Returns an AVA1 status; the reply body (or the cause of an
 * error) is in out / *out_len. Same shape as ava1_data_rpc(). An unknown method is
 * ERR_UNKNOWN_METHOD. */
int mgmt_rpc_dispatch(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap,
                      size_t *out_len);

/* ---- job.run operations wrapped around a management handler (P3 Task 5) ----
 *
 * An operation entry (the MGMT_OP* lines of mgmt_table.def) runs on an op job's worker: the
 * environment hook, the handler behind the capture sink, then the reply becomes the job's
 * result. Unlike a plain method, an `{"ok":false,...}` body is NOT turned into an error: the
 * operation ran and the body is its answer (fsck's non-zero code, a backup's err text), kept
 * verbatim for the caller that parses it. Only an ERROR frame fails the job (status from the
 * token, cause = the token). */
typedef struct {
    uint8_t op;            /* AVA1_JOB_OP_* */
    uint16_t legacy_frame; /* the legacy frame number (g_inflight_frame_type, the crash breadcrumb) */
    uint16_t ack_frame;
    uint32_t flags;        /* MGMT_* */
    mgmt_legacy_fn fn;
} mgmt_op_entry_t;

/* Registers the operations with ava1_op.c (after mgmt_rpc_install). 0, or -1. */
int mgmt_rpc_install_ops(const mgmt_op_entry_t *table, size_t n);

/* For a legacy handler running as an operation (no-ops anywhere else, e.g. on any other path):
 * has job.cancel arrived, and progress / totals for job.status. */
int mgmt_op_cancelled(void);
void mgmt_op_progress(uint64_t files, uint64_t bytes);
void mgmt_op_total(uint64_t files, uint64_t bytes);

/* ---- the capture sink (called from runtime.c's send_frame) ---- */
int mgmt_capture_active(void);
/* Records a frame instead of sending it. 0, or -1 when the body did not fit (the call
 * is then answered ERR_INTERNAL "reply truncated"). */
int mgmt_capture_frame(uint16_t frame_type, const void *body, uint64_t len);

/* ---- helpers for table runners ---- */

/* Calls `fn` under a capture sink holding up to `capture_cap` bytes. Returns 0 and fills
 * *rep with the success frame's body, or an AVA1 status with the cause in cx->out. */
int mgmt_legacy_call(mgmt_ctx_t *cx, mgmt_legacy_fn fn, const char *req, size_t req_len, size_t capture_cap,
                     mgmt_reply_t *rep);
/* The common MgmtText shape: the request is a MgmtText (or empty when !has_body), the reply is
 * the handler's body as a MgmtText. */
int mgmt_text_call(mgmt_ctx_t *cx, const uint8_t *req, uint32_t req_len, mgmt_legacy_fn fn, int has_body);
/* Writes `text` as an OK MgmtText reply (more = -1: absent). ERR_INTERNAL when it does not fit. */
int mgmt_reply_text(mgmt_ctx_t *cx, const char *text, size_t len, int more);
/* Writes `cause` as an error reply with `status`. Returns status. */
int mgmt_reply_error(mgmt_ctx_t *cx, int status, const char *cause);

/* The closest AVA1 error status for a legacy token (SPEC.md §7.3): ERR_PATH, ERR_NO_SPACE,
 * ERR_EXISTS, ERR_CROSS_DEVICE, ERR_BUSY, ERR_IO, ERR_UNKNOWN_JOB, ERR_CANCELLED,
 * ERR_PROTOCOL for malformed requests, else ERR_INTERNAL. */
int mgmt_status_for_token(const char *token);
/* 1 when `body` is a legacy failure: a JSON object whose TOP-LEVEL "ok" is false, wherever that key sits (an
 * "ok" inside a nested object or a string does not count). *token receives its top-level err/error/reason. */
int mgmt_legacy_failure(const char *body, size_t len, char *token, size_t token_cap);

/* Runner helpers the table (mgmt_table.def) names: (request, length, ctx, legacy handler). */
int mgmt_call_text(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn);   /* MgmtText in and out */
int mgmt_call_paged(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn);  /* MgmtText, offset/limit/more */
int mgmt_call_tail(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn);   /* MgmtText, clamped tail (log.klog, log.syslog) */
int mgmt_call_probe(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn);  /* MgmtText; {"ok":false,...} is an answer (net.reach) */
int mgmt_call_fs_mkdir(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn); /* FsMkdir -> empty */

int mgmt_call_text_keep(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn); /* a failure body is the cause */

int mgmt_call_empty(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn);     /* -> empty (node.shutdown) */
int mgmt_call_node_status(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn); /* -> NodeStatus */

/* Where a clamped tail of `len` bytes starts so that at most `cap` bytes remain: 0 and *start = 0
 * when it all fits; else 1 with *start on a line boundary (or a UTF-8 boundary). */
int mgmt_tail_window(const char *text, size_t len, size_t cap, size_t *start);

/* JSON string escaping for building a legacy request. Returns the length written (without
 * the NUL), or -1 when it does not fit. */
int mgmt_json_escape(const char *s, size_t n, char *out, size_t cap);
/* The unsigned integer after `"key":` in a JSON object: 1 found, 0 absent, -1 present but not an
 * unsigned integer (negative, fractional, a word) or too large for 64 bits. */
int mgmt_json_u64(const char *json, const char *key, uint64_t *out);

/* Pages the one JSON array in `json` ({"apps":[a,b,c]}): the prefix up to '[' and the suffix
 * from the last ']' are kept, elements [offset, offset+limit) are written, as many as fit in
 * `cap` (limit 0 = no limit). *more = 1 when elements remain after the last one written. A
 * single element that cannot fit is ERR_INTERNAL, never clipped. Returns 0 or an AVA1 status. */
int mgmt_page_json_array(const char *json, size_t len, uint64_t offset, uint64_t limit, char *out, size_t cap,
                         size_t *out_len, int *more);

#endif
