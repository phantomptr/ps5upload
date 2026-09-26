#ifndef PS5UPLOAD_INSTALLER_PROTOCOL_H
#define PS5UPLOAD_INSTALLER_PROTOCOL_H

#include <stddef.h>
#include <stdint.h>

#define INST_REQ_MAX   8192
#define INST_URL_MAX   2048
#define INST_PATH_MAX  1024
#define INST_HINT_MAX  256
#define INST_JOBID_MAX 64

typedef enum {
    INST_OP_UNKNOWN = 0,
    INST_OP_HELLO,
    INST_OP_INSTALL,
    INST_OP_JOB,
    INST_OP_STOP,
} inst_op_t;

typedef enum {
    INST_SRC_NONE = 0,
    INST_SRC_URL,
    INST_SRC_PATH,
} inst_src_t;

typedef struct {
    inst_op_t  op;
    inst_src_t src;
    char       url[INST_URL_MAX];
    char       path[INST_PATH_MAX];
    char       name_hint[INST_HINT_MAX];
    char       job[INST_JOBID_MAX];
    /* NULL on success; otherwise our string error code ("bad_request"). */
    const char *error;
} inst_request_t;

/* Parse one JSON-lines request. Never reads past `len`; a request longer
 * than INST_REQ_MAX, malformed JSON, an unknown/absent op, or an install
 * that does not carry exactly one of url/path sets req->error="bad_request".
 * Always fully initializes *req. */
void inst_parse_request(const char *buf, size_t len, inst_request_t *req);

/* Reply builders. Each writes a single JSON object (no trailing newline;
 * the accept loop appends "\n"). Return bytes written excluding the NUL,
 * or -1 if `cap` is too small. */
int inst_reply_hello(char *out, size_t cap, const char *version,
                     const char *fw, const char *state,
                     uint32_t init_rc, int escalated);
int inst_reply_install_ok(char *out, size_t cap, const char *job,
                          const char *via);
int inst_reply_job(char *out, size_t cap, const char *phase,
                   uint64_t bytes_served, uint64_t total, uint32_t code);
int inst_reply_ok(char *out, size_t cap);
int inst_reply_err_busy(char *out, size_t cap, const char *job);
int inst_reply_err_not_ready(char *out, size_t cap, uint32_t init_rc);
int inst_reply_err_str(char *out, size_t cap, const char *error);
int inst_reply_err_sony(char *out, size_t cap, uint32_t code, const char *hint);

/* Pure boot-wait decision: 1 = perform the 25s wait, 0 = skip. Skips when
 * system uptime is already over 120 seconds. */
int inst_should_boot_wait(double uptime_seconds);

#endif /* PS5UPLOAD_INSTALLER_PROTOCOL_H */
