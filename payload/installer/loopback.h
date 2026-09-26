#ifndef PS5UPLOAD_INSTALLER_LOOPBACK_H
#define PS5UPLOAD_INSTALLER_LOOPBACK_H

#include <stddef.h>
#include <stdint.h>

typedef struct inst_loopback inst_loopback_t;

/* Start a 127.0.0.1:0 HTTP server serving `pkg_path` at /<attempt>/<basename>.
 * Records the file size as total and the current time as the job's fixed
 * Last-Modified. On success returns 0, sets *out_port to the ephemeral port,
 * and *out to the running server. -1 on error (nothing left running). */
int inst_loopback_start(inst_loopback_t **out, const char *pkg_path,
                        const char *attempt, const char *basename,
                        uint16_t *out_port);

uint64_t inst_loopback_bytes_served(inst_loopback_t *lb);
uint64_t inst_loopback_total(inst_loopback_t *lb);
double   inst_loopback_idle_seconds(inst_loopback_t *lb);
void     inst_loopback_stop(inst_loopback_t *lb);

#endif /* PS5UPLOAD_INSTALLER_LOOPBACK_H */
