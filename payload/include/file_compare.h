#ifndef PS5UPLOAD_FILE_COMPARE_H
#define PS5UPLOAD_FILE_COMPARE_H

/* Whether two files' contents differ: 1 they differ (or their sizes do), 0 the same bytes,
 * -1 one could not be read. Reads both in chunks from a heap buffer (no large stack frame). */
int file_contents_differ(const char *a, const char *b);

#endif
