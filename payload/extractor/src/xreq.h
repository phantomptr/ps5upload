/* The extractor's two files: the request the helper writes and the status the extractor writes back.
 *
 * Request (written by the helper with mode 0600, deleted by the extractor once read):
 *   {"id":"<hex>","archive":"/abs/first-volume","dest":"/abs/folder","password":"...","merge":false,"replace":false,
 *    "list":false,"top":"from/to/..."}   (list and top optional: see xjob.h)
 * Status (rewritten through a temporary file and a rename in the same folder):
 *   {"id","pid","state":"running|done|failed","files_done","files_total","bytes_done","bytes_total",
 *    "files","dirs","bytes","skipped","unsafe","links","created_dest","current","message"
 *    [,"top":"a/b/c","top_dirs":"a","top_more":false when the request listed]}
 * `message` is empty or the cause token of a failure (fs_extract_...). The helper's side is
 * payload/src/archive_extract.c. */
#ifndef PS5UPLOAD_XREQ_H
#define PS5UPLOAD_XREQ_H

#include <stddef.h>

#include "xjob.h"

#ifdef __cplusplus
extern "C" {
#endif

int xreq_parse(const char *json, xjob_t *j);
int xreq_read_file(const char *path, xjob_t *j);
int xstatus_format(char *out, size_t cap, const xjob_t *j, const char *state, long pid);
int xstatus_write(const char *path, const xjob_t *j, const char *state, long pid);

#ifdef __cplusplus
}
#endif

#endif
