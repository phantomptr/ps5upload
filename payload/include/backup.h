#ifndef PS5UPLOAD_BACKUP_H
#define PS5UPLOAD_BACKUP_H

#include <stdint.h>
#include <stddef.h>

/* backup_snapshot / backup_restore return this when a job.run cancel stopped them. */
#define BACKUP_CANCELLED (-2)

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

/* What a restore did with the snapshot's manifest lines. */
typedef struct {
    int restored; /* files written back to their paths */
    int skipped;  /* lines not used: no tab, a path outside the writable roots, a bad name, too long */
    int failed;   /* files that could not be copied back (missing in the snapshot, a write error) */
} backup_restore_counts_t;

/* Restore a snapshot by tag+timestamp. Returns 0 when its manifest was read to the end (whatever
 * each line came to: see `out`), -1 when the snapshot or its manifest is missing, BACKUP_CANCELLED.
 * `out` (may be NULL) holds the counts so far in every case. */
int backup_restore_ex(const char *tag, int64_t timestamp, backup_restore_counts_t *out);

/* backup_restore_ex with only the restored count. */
int backup_restore(const char *tag, int64_t timestamp, int *out_restored);

/* The BACKUP_RESTORE reply for a restore that returned `rc` with `c`: `tag_json` is the tag already
 * JSON-escaped. ok is true only when at least one file was restored; "partial" is true when some
 * lines were not restored too. A restore that restored nothing (an empty manifest, every line
 * skipped or failed) is ok:false with a reason in "err". Returns the length written (snprintf). */
int backup_restore_reply(char *out, size_t cap, const char *tag_json, int rc,
                         const backup_restore_counts_t *c);

/* Delete a snapshot by tag+timestamp. Returns 0 on success. */
int backup_delete(const char *tag, int64_t timestamp);

/* Initialize backup root directory (idempotent). */
void backup_init(void);

#endif /* PS5UPLOAD_BACKUP_H */
