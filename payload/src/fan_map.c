#include "fan_map.h"

#include <stdlib.h>
#include <string.h>

/* Parse the integer after `key` (e.g. "\"temp_c\":") searching [from, end).
 * Returns 0 and sets *out, or -1. Minimal scanner in the style of runtime.c's
 * extract_json_*: no JSON parser is linked into the payload. */
static int scan_int(const char *from, const char *end, const char *key, int *out, const char **after) {
    const char *p = strstr(from, key);
    if (!p || (end && p >= end)) return -1;
    p += strlen(key);
    while (*p == ' ' || *p == '\t') p++;
    if (*p < '0' || *p > '9') return -1;
    *out = (int)strtol(p, (char **)after, 10);
    return 0;
}

int fan_map_threshold(const char *json) {
    if (!json) return -1;
    static const char tkey[] = "\"temp_c\":";
    static const char dkey[] = "\"duty_pct\":";
    int seen = 0;
    int best = -1; /* lowest temp asking for 100% */
    const char *cur = json;
    for (;;) {
        int temp;
        const char *after;
        if (scan_int(cur, NULL, tkey, &temp, &after) != 0) break;
        const char *next = strstr(after, tkey); /* bound: this point's own fields */
        /* duty may precede temp_c in the object; look from the start of this
         * object (last '{' before temp_c) to the next point's temp_c. */
        const char *obj = after;
        while (obj > json && *obj != '{') obj--;
        int duty = -1;
        const char *dummy;
        const char *dpos = strstr(obj, dkey);
        const char *close = strchr(obj, '}'); /* this object's own fields only */
        if (dpos && (!close || dpos < close)) (void)scan_int(obj, close, dkey, &duty, &dummy);
        seen++;
        if (duty >= 100 && (best < 0 || temp < best)) best = temp;
        if (!next) break;
        cur = after;
    }
    if (!seen) return -1;
    /* Never 100%, or only above what we may set: the console's own setting is the closest
     * thing to what was asked, and it is quieter than anything we could apply instead. */
    if (best < 0 || best > FAN_MAP_MAX_C) return FAN_MAP_CONSOLE_OWN;
    return best < FAN_MAP_MIN_C ? FAN_MAP_MIN_C : best;
}
