#ifndef PS5UPLOAD_ACTIVITY_H
#define PS5UPLOAD_ACTIVITY_H

#include <stddef.h>
#include <stdint.h>

void activity_init(void);

/* Persist tracked play time now (called on orderly shutdown). */
void activity_flush(void);

/* Discard all recorded play time. Returns titles removed. */
int activity_reset(void);

int activity_get_json(char *buf, size_t cap, size_t *written);

int activity_db_query_json(const char *query, char *buf, size_t cap,
                           size_t *written);

#endif
