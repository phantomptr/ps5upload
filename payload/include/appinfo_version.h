#ifndef PS5UPLOAD_APPINFO_VERSION_H
#define PS5UPLOAD_APPINFO_VERSION_H

/* A title's installed version from its appinfo.db rows (#934).
 *
 * A PS5 title says CONTENT_VERSION ("01.000.016"). A PS4 title says APP_VER and VERSION: an
 * installed update raises VERSION ("01.32") while APP_VER stays at the base game's "01.00"
 * (RDR2 CUSA03041, ELDEN RING CUSA28527), so the higher of the two is the version it runs.
 * Every helper reader (the game list, the cheat version, clean-up) asks for these keys and picks
 * the same way; the engine (AppInfoRows::installed_version) and the pages
 * (installedAppVersion in client/src/lib/versionNumbers.ts) do too.
 *
 * Header-only and pure, so the host tests include it as is. */

#include <stddef.h>

/* For an APP_INFO_QUERY "keys" list, and for an SQL IN (...) list. */
#define APPINFO_VERSION_KEYS "CONTENT_VERSION,VERSION,APP_VER"
#define APPINFO_VERSION_SQL_KEYS "'CONTENT_VERSION','VERSION','APP_VER'"

/* Dotted versions compared number by number ("01.32" above "01.05", "1.10" above "1.9"). Leading
 * spaces of a part are skipped; anything else that is not a digit or a dot ends the comparison
 * there. <0, 0 or >0. */
static inline int appinfo_version_cmp(const char *a, const char *b) {
    while ((a && *a) || (b && *b)) {
        unsigned long x = 0, y = 0;
        while (a && *a == ' ') a++;
        while (b && *b == ' ') b++;
        for (; a && *a >= '0' && *a <= '9'; a++)
            if (x < 100000000ul) x = x * 10ul + (unsigned long)(*a - '0');
        for (; b && *b >= '0' && *b <= '9'; b++)
            if (y < 100000000ul) y = y * 10ul + (unsigned long)(*b - '0');
        if (x != y) return x < y ? -1 : 1;
        if (a && *a == '.') a++;
        else a = NULL; /* the end, or junk: nothing more to compare */
        if (b && *b == '.') b++;
        else b = NULL;
    }
    return 0;
}

/* The installed version: CONTENT_VERSION when there is one, else the higher of VERSION and
 * APP_VER. Any argument may be NULL or "" (absent). Returns one of the arguments, or "" when all
 * are absent. */
static inline const char *appinfo_version_pick(const char *content, const char *version,
                                               const char *app_ver) {
    int hv = version && version[0], ha = app_ver && app_ver[0];
    if (content && content[0]) return content;
    if (hv && ha) return appinfo_version_cmp(version, app_ver) >= 0 ? version : app_ver;
    if (hv) return version;
    if (ha) return app_ver;
    return "";
}

#endif
