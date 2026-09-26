#ifndef PS5UPLOAD_INSTALLER_HTTP_RANGE_H
#define PS5UPLOAD_INSTALLER_HTTP_RANGE_H

#include <stddef.h>
#include <stdint.h>
#include <time.h>

#define INST_HTTP_TARGET_MAX 1024

typedef enum {
    INST_RANGE_NONE  = 0,   /* no Range header -> serve full 200 */
    INST_RANGE_OK    = 1,   /* *start,*end set, inclusive */
    INST_RANGE_UNSAT = -1,  /* -> 416 */
} inst_range_result_t;

/* Parse a single Range header value ("bytes=a-b" | "a-" | "-n"). A
 * multi-range (comma) or a non-"bytes=" unit returns INST_RANGE_NONE so the
 * caller serves the whole file. End is clamped to total-1. total==0 makes any
 * concrete range unsatisfiable. */
inst_range_result_t inst_parse_range(const char *range_value, uint64_t total,
                                     uint64_t *start, uint64_t *end);

typedef struct {
    char method[8];
    char target[INST_HTTP_TARGET_MAX]; /* percent-decoded, query stripped */
    int  is_get;
    int  is_head;
    int  keep_alive;
} inst_http_req_t;

/* Parse the request line + Connection header. 0 on success, -1 if the
 * request line is malformed (no "METHOD SP TARGET SP VERSION"). */
int inst_http_parse_head(const char *req, size_t len, inst_http_req_t *out);

/* Case-insensitive header value into out (cap). 1 found, 0 absent. */
int inst_http_header(const char *req, const char *name, char *out, size_t cap);

/* Percent-decode a URL path into out (cap). 0 success, -1 overflow/malformed. */
int inst_http_percent_decode(const char *in, char *out, size_t cap);

/* Format epoch seconds as an IMF-fixdate. 0 success, -1 overflow. */
int inst_http_imf_date(time_t t, char *out, size_t cap);

/* Response-header builders. `date` is the live clock; `fixed_lm` is the job's
 * fixed Last-Modified. Return bytes written excluding NUL, or -1 on overflow. */
int inst_http_hdr_200(char *out, size_t cap, uint64_t total,
                      time_t date, time_t fixed_lm, int keep_alive);
int inst_http_hdr_206(char *out, size_t cap, uint64_t start, uint64_t end,
                      uint64_t total, time_t date, time_t fixed_lm, int keep_alive);
int inst_http_hdr_416(char *out, size_t cap, uint64_t total, int keep_alive);
int inst_http_hdr_404(char *out, size_t cap, int keep_alive);

#endif /* PS5UPLOAD_INSTALLER_HTTP_RANGE_H */
