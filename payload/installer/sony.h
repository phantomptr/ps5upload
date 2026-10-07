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

/* Bounded sceAppInstUtilInitialize (reuses timed_init.h). Caches success.
 * Returns 0 / PS5_TIMED_INIT_TIMEOUT / Sony rc; updates st->init_done/init_rc. */
int inst_sony_init(inst_sony_state_t *st);

typedef struct {
    int         accepted;  /* 1 when Sony rc == 0 */
    uint32_t    code;      /* Sony rc (0 on accept) */
    const char *via;       /* "url" | "loopback" | "path" | "file" */
    const char *hint;      /* non-NULL for a known error code */
} inst_install_result_t;

/* Run the Sony install for an already-parsed, already-validated request.
 * INST_SRC_URL: one InstallByPackage(url), via="url".
 * INST_SRC_PATH: start the loopback server, InstallByPackage(loopback url),
 *   via="loopback"; on a network-class refusal
 *   ((code & 0xFFFF0000)==0x80430000) stop the loopback and retry ONCE with
 *   the bare rewritten path (FW<11 swaps to ShellCore for that call),
 *   via="path". *lb_out receives the running loopback only for a live
 *   via="loopback" result; NULL otherwise. Caller owns *lb_out thereafter. */
inst_install_result_t inst_sony_install(const inst_request_t *req,
                                        inst_sony_state_t *st,
                                        inst_loopback_t **lb_out,
                                        const char *attempt);

#endif /* PS5UPLOAD_INSTALLER_SONY_H */
