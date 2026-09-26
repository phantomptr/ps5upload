#ifndef PS5UPLOAD_FTP_FORMAT_H
#define PS5UPLOAD_FTP_FORMAT_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

/* Pure response formatting for the payload's FTP server.
 *
 * Split out from ftp_server.c so the wire format is testable on the host
 * without sockets — the socket plumbing stays there, the string shapes
 * live here. Same header-only pattern as hw_guard.h and appdb_scan.h.
 * Tests: payload/tests/ftp_format_selftest.c. */

/* What a client asked for with EPSV. */
typedef enum {
    FTP_EPSV_IPV4 = 0,  /* no argument, or "1" — reply 229 */
    FTP_EPSV_ALL,       /* "ALL" — reply 200, client promises EPSV only */
    FTP_EPSV_BAD_PROTO, /* "2" or anything else — reply 522 */
} ftp_epsv_arg_t;

static inline ftp_epsv_arg_t ftp_parse_epsv_arg(const char *arg) {
    if (!arg) return FTP_EPSV_IPV4;
    while (*arg == ' ') arg++;
    if (*arg == '\0') return FTP_EPSV_IPV4;
    if (strcasecmp(arg, "ALL") == 0) return FTP_EPSV_ALL;
    if (strcmp(arg, "1") == 0) return FTP_EPSV_IPV4;
    /* "2" is IPv6; our data socket is AF_INET, so refuse rather than
     * hand back a port the client cannot reach. */
    return FTP_EPSV_BAD_PROTO;
}

/* "Entering Passive Mode (192,168,86,100,244,183)." */
static inline int ftp_format_pasv(const unsigned char ip[4], int port, char *out,
                                  size_t cap) {
    if (!ip || !out || cap == 0 || port < 0 || port > 65535) return -1;
    int n = snprintf(out, cap, "Entering Passive Mode (%u,%u,%u,%u,%u,%u).",
                     ip[0], ip[1], ip[2], ip[3], (unsigned)(port >> 8) & 0xFF,
                     (unsigned)port & 0xFF);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

/* "Entering Extended Passive Mode (|||62647|)" — RFC 2428 §3. */
static inline int ftp_format_epsv(int port, char *out, size_t cap) {
    if (!out || cap == 0 || port < 0 || port > 65535) return -1;
    int n = snprintf(out, cap, "Entering Extended Passive Mode (|||%d|)", port);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

/* Ten-character mode string, e.g. "drwxr-xr-x". */
static inline void ftp_format_mode(int is_dir, unsigned int mode, char out[11]) {
    out[0] = is_dir ? 'd' : '-';
    out[1] = (mode & 0400) ? 'r' : '-';
    out[2] = (mode & 0200) ? 'w' : '-';
    out[3] = (mode & 0100) ? 'x' : '-';
    out[4] = (mode & 0040) ? 'r' : '-';
    out[5] = (mode & 0020) ? 'w' : '-';
    out[6] = (mode & 0010) ? 'x' : '-';
    out[7] = (mode & 0004) ? 'r' : '-';
    out[8] = (mode & 0002) ? 'w' : '-';
    out[9] = (mode & 0001) ? 'x' : '-';
    out[10] = '\0';
}

/* One LIST line, without the trailing CRLF. */
static inline int ftp_format_list_line(int is_dir, unsigned int mode,
                                       long long size, const char *timestr,
                                       const char *name, char *out, size_t cap) {
    if (!timestr || !name || !out || cap == 0) return -1;
    char perm[11];
    ftp_format_mode(is_dir, mode, perm);
    int n = snprintf(out, cap, "%s 1 root root %lld %s %s", perm, size, timestr,
                     name);
    /* Truncation would hand the client a different filename than the one
     * on disk, so refuse instead. */
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

#endif
