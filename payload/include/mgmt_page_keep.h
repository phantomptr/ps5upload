/* The last paged method's whole answer, kept for the pages after the first.
 *
 * A paged method's handler builds its entire answer and a window is cut from it, so a list spanning
 * N replies used to walk the saves, screenshots or apps N times. A client reading pages in turn now
 * gets them from one walk, and so from one consistent list.
 *
 * Page 0 always runs the handler (a new look), and an answer is only kept while there are pages
 * left, so at most one (up to 1 MiB) is held, and not for long. Thread-safe. */
#ifndef PS5UPLOAD_MGMT_PAGE_KEEP_H
#define PS5UPLOAD_MGMT_PAGE_KEEP_H

#include <stddef.h>
#include <stdint.h>

/* An answer older than this is walked again. */
#define MGMT_PAGE_KEEP_MS 5000u

/* Keeps a copy of `body` as the answer to (fn, user) at `now_ms`, replacing what was kept. Out of
 * memory just keeps nothing. `user`: what else selects the answer (the request's user id, or
 * UINT64_MAX for none). */
void mgmt_page_keep_store(const void *fn, uint64_t user, const char *body, size_t len, uint64_t now_ms);

/* The kept answer to (fn, user), if it is fresh: a copy in *out (free() it) and its length in
 * *len, returns 1. Otherwise 0. */
int mgmt_page_keep_take(const void *fn, uint64_t user, uint64_t now_ms, char **out, size_t *len);

/* Drops what is kept (the last page was served). */
void mgmt_page_keep_clear(void);

#endif
