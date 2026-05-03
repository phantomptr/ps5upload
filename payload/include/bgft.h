/*
 * bgft.h — payload-side bindings for Sony's Background File Transfer
 *          (BGFT) service. Used by the PKG_INSTALL frame handler to
 *          drive a `.pkg` install over an HTTP URL the host serves.
 *
 * BGFT is part of stock PS5 firmware — the same service the console's
 * own PSN downloads use. Calling its private API surface from our
 * payload (with debugger authid set by kstuff) lets us install `.pkg`
 * files without depending on any third-party RPI listener. Sony's
 * installer does the actual decryption + install using the device's
 * own keys; we just hand it a URL and a content_id.
 *
 * Lifecycle (called from the PKG_INSTALL frame handler):
 *   1. bgft_install_start(url, content_id, size, title, package_type,
 *                          &task_id, &err_code)
 *      → registers + starts a BGFT task. Synchronous; returns once
 *        the task is queued in BGFT. The actual download happens
 *        asynchronously inside Sony's service.
 *   2. bgft_install_status(task_id, &phase, &downloaded, &total,
 *                           &err_code)
 *      → polls the task. Cheap; safe to call every 1 s.
 *   3. (no explicit "stop" — once the host stops serving HTTP, BGFT
 *       sees the stream drop and surfaces a download error in the PS5
 *       notifications. Cancellation is host-side.)
 *
 * Thread safety: serialized via a single mutex. BGFT itself is
 * single-threaded; concurrent `RegisterTask` callers will hit kernel
 * locks and deadlock, so we hold the mutex for the duration of every
 * call.
 *
 * Firmware compatibility: the Sony stubs are resolved via dlopen +
 * dlsym at first use. If `libSceBgft.sprx` isn't loadable (or any
 * required symbol is missing), `bgft_install_start` returns -1 with
 * `err_code` set to a sentinel and `bgft_install_unavailable_reason`
 * yields a string the engine surfaces to the UI.
 */

#ifndef PS5UPLOAD2_BGFT_H
#define PS5UPLOAD2_BGFT_H

#include <stdint.h>

/* Phase enum — matches `ps5upload_core::pkg_install::InstallPhase`
 * on the host side, kept as plain ints here so the payload header
 * stays plain C without needing a shared schema crate. */
typedef enum {
    BGFT_PHASE_QUEUED   = 0,
    BGFT_PHASE_DOWNLOAD = 1,
    BGFT_PHASE_INSTALL  = 2,
    BGFT_PHASE_DONE     = 3,
    BGFT_PHASE_ERROR    = 4,
} bgft_phase_t;

/* Sentinel err_codes layered on top of Sony's 0x80990xxx family.
 * High bit set so they don't collide with anything Sony returns.
 * Kept in sync with the host's err_code_message() table. */
#define BGFT_ERR_LIB_NOT_LOADABLE  0xE0000001u
#define BGFT_ERR_SYMBOL_MISSING    0xE0000002u
#define BGFT_ERR_INIT_FAILED       0xE0000003u
#define BGFT_ERR_REGISTER_FAILED   0xE0000004u
#define BGFT_ERR_START_FAILED      0xE0000005u
#define BGFT_ERR_UCRED_ELEVATE     0xE0000006u
/* Synthetic task table is full — too many in-flight installs. Reset
 * the tx table or wait for current installs to drain. */
#define BGFT_ERR_TASK_TABLE_FULL   0xE0000007u

/* Register + start a BGFT install task.
 *
 * `url`           HTTP URL Sony's BGFT will fetch the .pkg from.
 * `content_id`    36-byte content_id from the PKG header.
 * `size`          total bytes (sum across split parts when applicable).
 * `title`         display string for PS5 notifications.
 * `package_type`  BGFT package_type (e.g. "PS4GD", "PS4AC").
 * `out_task_id`   on success: BGFT's task_id, used for status polling.
 * `out_err_code`  Sony's BGFT err_code (0 on success) or a BGFT_ERR_*
 *                 sentinel on pre-call machinery failure.
 *
 * Returns 0 on success, -1 on any failure with `*out_err_code` set. */
int bgft_install_start(const char *url,
                        const char *content_id,
                        uint64_t size,
                        const char *title,
                        const char *package_type,
                        int32_t *out_task_id,
                        uint32_t *out_err_code);

/* Poll an in-flight task. Returns 0 on success with all out-params
 * populated; -1 if the task isn't found or BGFT is unavailable. */
int bgft_install_status(int32_t task_id,
                         bgft_phase_t *out_phase,
                         uint64_t *out_downloaded,
                         uint64_t *out_total,
                         uint32_t *out_err_code);

/* Diagnostic string for the most recent init failure. NULL when init
 * succeeded or hasn't been attempted yet. Lifetime: static; safe to
 * read from any thread, but the string only describes the most recent
 * init outcome. */
const char *bgft_install_unavailable_reason(void);

#endif /* PS5UPLOAD2_BGFT_H */
