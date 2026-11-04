#ifndef PS5UPLOAD_INSTALLER_SONY_H
#define PS5UPLOAD_INSTALLER_SONY_H

#include <stdint.h>
#include "protocol.h"
#include "loopback.h"

typedef struct {
    int      escalated;
    int      fw_major;
    int      init_done;
    uint32_t init_rc;
} inst_sony_state_t;

/* Bounded sceAppInstUtilInitialize (inst_timed_init.h, monotonic). Caches success.
 * Returns 0 / INST_TIMED_INIT_TIMEOUT / Sony rc; updates st->init_done/init_rc. */
int inst_sony_init(inst_sony_state_t *st);

typedef struct {
    int         accepted;  /* 1 when Sony rc == 0 */
    uint32_t    code;      /* Sony rc (0 on accept) */
    const char *via;       /* "url" | "loopback" | "path" | "file" */
    const char *hint;      /* non-NULL for a known error code */
    const char *error;     /* our own reason when Sony was never called, else NULL */
} inst_install_result_t;

/* Run the Sony install for an already-parsed, already-validated request.
 * INST_SRC_URL: one InstallByPackage(url), via="url".
 * INST_SRC_PATH with route "path": the plain-path forms, via="path"|"file".
 * INST_SRC_PATH otherwise: start the loopback server, InstallByPackage(loopback
 *   url), via="loopback". A refusal is returned as it is, whatever its class,
 *   and nothing else is tried: this daemon cannot tell a patch from a base game,
 *   and the plain path is the route that has wiped installed games, so only the
 *   engine's guard may ask for it. .error="source_unreadable" when the file cannot
 *   be opened, "loopback_failed" when the server would not start.
 * *lb_out receives the running loopback only for a live via="loopback" result;
 * NULL otherwise. Caller owns *lb_out thereafter. */
inst_install_result_t inst_sony_install(const inst_request_t *req,
                                        inst_sony_state_t *st,
                                        inst_loopback_t **lb_out,
                                        const char *attempt);

#endif /* PS5UPLOAD_INSTALLER_SONY_H */
