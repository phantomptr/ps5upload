#ifndef PS5UPLOAD_REMOTEPLAY_H
#define PS5UPLOAD_REMOTEPLAY_H

#include <stdint.h>
#include <stddef.h>

void remoteplay_init(void);

int remoteplay_request(const char *manual_account_id);

int remoteplay_get_status(char *buf, size_t cap);

/* The current PIN and account id, with NO pairing-completion probing.
 *
 * remoteplay_get_status() drives sceRemoteplayConfirmDeviceRegist as a
 * side effect, which finalises a pending registration on the console. A
 * client that is about to perform the registration itself must therefore
 * never poll status to learn the PIN — it would consume the very pairing
 * it is trying to complete. This is that read, and nothing else. */
int remoteplay_pin_snapshot(char *out, size_t out_size);

int remoteplay_cancel(void);

/* Read-only readiness snapshot as JSON. Returns the snprintf length, or
 * negative on error. Performs no writes. */
int remoteplay_readiness_json(char *out, size_t out_size);

/* Paired devices from the 32-slot registration table, as JSON. Pairing
 * secrets are never included. Returns the length written. */
int remoteplay_devices_json(char *out, size_t out_size);

/* Read-only layout probe of one pairing record.
 *
 * Reports each entry's type and width, never its contents — the regist and
 * AES keys are pairing secrets and this project is public. Exists so that
 * nothing has to write to the table while guessing its shape. */
int remoteplay_regist_probe_json(char *out, size_t out_size);

/* Enable Remote Play. user_scope=0 is the system service toggle,
 * user_scope=1 is per-user permission (FW 10.00+). Writes the re-read
 * readiness snapshot to `out`. Returns >=0 on success, -1 write failed,
 * -2 no per-user setting on this firmware, -3 no foreground user. */
int remoteplay_enable(int user_scope, char *out, size_t out_size);

#endif
