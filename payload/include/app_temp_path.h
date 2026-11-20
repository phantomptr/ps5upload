/* A running game's temporary-data folder: `/app_temp<N>` (one or two digits) and what is below it.
 *
 * The PS5 mounts each running app's temp storage there (for example
 * `/app_temp0/PPSA17221/user<id>/games/com.mojang`), and some games' add-on guides have files copied
 * into it while the game runs (#889). It is a writable data area like /data and /user, so the
 * helper's write rule accepts it; every other guard (no `.`/`..` components, the canonical-path
 * re-check, the protected folders) still applies on top. Pure string check, no I/O. 1 = yes. */
#ifndef PS5UPLOAD_APP_TEMP_PATH_H
#define PS5UPLOAD_APP_TEMP_PATH_H

#include <string.h>

static inline int is_app_temp_path(const char *p) {
    static const char PRE[] = "/app_temp";
    if (!p || strncmp(p, PRE, sizeof(PRE) - 1) != 0) return 0;
    const char *q = p + sizeof(PRE) - 1;
    if (*q < '0' || *q > '9') return 0;
    q++;
    if (*q >= '0' && *q <= '9') q++;
    return *q == '\0' || *q == '/';
}

#endif
