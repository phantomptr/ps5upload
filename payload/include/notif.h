#ifndef PS5UPLOAD_NOTIF_H
#define PS5UPLOAD_NOTIF_H

#include <stdint.h>
#include <stddef.h>

typedef enum {
    NOTIF_LEVEL_INFO  = 0,
    NOTIF_LEVEL_WARN  = 1,
    NOTIF_LEVEL_ERROR = 2,
} notif_level_t;

/* Load the persisted notification ring from disk. Call once at payload
 * startup (before any client connects) so notifications pushed in a
 * previous payload lifetime survive a redeploy — the whole point of an
 * on-PS5 store vs. the client-side inbox. Safe to call when no file
 * exists (starts empty) and idempotent enough for a single boot call. */
void notif_init(void);

int notif_list(uint64_t since_seq, char *buf, size_t cap,
               size_t *written);

int notif_send(const char *msg, int level);

/* Empty the notification ring and its on-disk snapshot.
 * Returns how many entries were removed. */
int notif_clear(void);

#endif
