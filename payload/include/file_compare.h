#ifndef PS5UPLOAD_FILE_COMPARE_H
#define PS5UPLOAD_FILE_COMPARE_H

#include <stddef.h>

/* Whether two files' contents differ: 1 they differ (or their sizes do), 0 the same bytes,
 * -1 one could not be read. Reads both in chunks from a heap buffer (no large stack frame). */
int file_contents_differ(const char *a, const char *b);

/* Keeps an open save's edited copy that could not be written back: renames `path` to the first
 * of `<path>.unsaved`, `<path>.2.unsaved` .. `<path>.99.unsaved` that does not exist yet, so a
 * copy kept earlier (another game's save image of the same name, or the same save's) is never
 * replaced, which rename() alone does without a word. The same folder, so the rename never
 * crosses a device. 0 with the new name in `kept`; -1 when it stays where it is, `kept` then
 * naming `path`. */
int keep_aside(const char *path, char *kept, size_t kept_sz);

#endif
