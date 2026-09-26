#ifndef PS5UPLOAD_INSTALLER_PATHSAFE_H
#define PS5UPLOAD_INSTALLER_PATHSAFE_H

#include <stddef.h>
#include <stdint.h>

/* Max local path length the daemon accepts (equal to INST_PATH_MAX from
 * protocol.h, declared locally so pathsafe.c has no dependency on it). */
#define INST_PATH_MAX_SAFE 1024

/* 1 if `path` is a safe staged-pkg path: absolute, no ".." path component,
 * no trailing slash, ends ".pkg" (case-insensitive), and rooted under
 * /data/ | /user/ | /mnt/usb<N>/ | /mnt/ext<N>/ (N one or more digits).
 * Does NOT stat the file. 0 otherwise (incl. NULL/empty). */
int inst_path_is_safe(const char *path);

/* Rewrite a /data/... path to /user/data/...; copy anything else verbatim.
 * Returns 0 on success, -1 on overflow. */
int inst_rewrite_path(const char *in, char *out, size_t cap);

/* Pointer to the basename (after the last '/'), or `path` if no '/'. */
const char *inst_basename(const char *path);

/* Percent-encode `in` for a URL path segment: A-Za-z0-9-._~ pass through,
 * everything else becomes %XX (uppercase hex). 0 on success, -1 on overflow. */
int inst_percent_encode(const char *in, char *out, size_t cap);

/* Build http://127.0.0.1:<port>/<attempt>/<percent-encoded basename>.
 * Returns bytes written excluding NUL, or -1 on overflow. */
int inst_build_loopback_url(char *out, size_t cap, uint16_t port,
                            const char *attempt, const char *basename);

/* 1 if `code` is a Sony network-class error: (code & 0xFFFF0000)==0x80430000. */
int inst_is_network_class(uint32_t code);

#endif /* PS5UPLOAD_INSTALLER_PATHSAFE_H */
