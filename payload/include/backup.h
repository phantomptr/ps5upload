#ifndef PS5UPLOAD_BACKUP_H
#define PS5UPLOAD_BACKUP_H

#include <stdint.h>
#include <stddef.h>

/* Validate tag name: [a-zA-Z0-9_-], max 32 chars. Returns 0 if valid. */
int backup_validate_tag(const char *tag);

/* Snapshot a file or directory tree under /data/ps5upload/backups/<tag>/<ts>/.
 * Returns 0 on success, -1 on error. On success, fills out_timestamp,
 * out_files, out_bytes. */
int backup_snapshot(const char *tag, const char *src_path,
                    int64_t *out_timestamp, int *out_files,
                    uint64_t *out_bytes);

/* List snapshots as JSON: {"snapshots":[{tag,timestamp,files,bytes},...]}.
 * tag_filter="" or NULL lists all tags. Writes into buf, returns 0. */
int backup_list(const char *tag_filter, char *buf, size_t cap,
                size_t *written);

/* Restore a snapshot by tag+timestamp. Returns 0 on success. */
int backup_restore(const char *tag, int64_t timestamp, int *out_restored);

/* Delete a snapshot by tag+timestamp. Returns 0 on success. */
int backup_delete(const char *tag, int64_t timestamp);

/* Initialize backup root directory (idempotent). */
void backup_init(void);

#endif /* PS5UPLOAD_BACKUP_H */
