#ifndef PS5UPLOAD_TMDB_H
#define PS5UPLOAD_TMDB_H

#include <stddef.h>

void tmdb_init(void);

int tmdb_fetch(const char *id, int refresh,
               char *buf, size_t cap, size_t *written);

int tmdb_store(const char *id, const char *json, size_t len);

#endif
