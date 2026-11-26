#ifndef PS5UPLOAD_JSON_KEY_H
#define PS5UPLOAD_JSON_KEY_H

#include <string.h>

/* The first place `quoted` (a JSON string with its quotes, e.g. "\"en-US\"") appears in `json` as
 * an object KEY, that is followed by ':' (spaces allowed between), or NULL. A plain strstr also
 * finds it as a VALUE: in a param.json written with its keys sorted, "defaultLanguage": "en-US"
 * comes before the "en-US" block, and the title read after it was el-GR's or en-GB's. */
static inline const char *json_find_key(const char *json, const char *quoted) {
    size_t n = strlen(quoted);
    const char *p = json;
    if (n == 0) return NULL;
    while ((p = strstr(p, quoted)) != NULL) {
        const char *q = p + n;
        while (*q == ' ' || *q == '\t' || *q == '\r' || *q == '\n') q++;
        if (*q == ':') return p;
        p += n;
    }
    return NULL;
}

#endif /* PS5UPLOAD_JSON_KEY_H */
