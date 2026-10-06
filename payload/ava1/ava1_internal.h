/* Helpers shared by the receiver's C files (ava1_apply.c, ava1_recv.c): one copy each.
 * Not part of any interface another task builds against. */
#ifndef AVA1_INTERNAL_H
#define AVA1_INTERNAL_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#include "ava1_b3.h"
#include "ava1_job.h"

/* Every file and folder an upload writes on the console is 0777, whatever mode the computer
 * sent (Windows has none and sends 0644). The PS5's app loader refuses game files without
 * world-execute ("can't start the game or app"), which is why the 5.x helper forced 0777 too.
 * main() sets umask(0), so these land as written. */
#define AVA1_CONSOLE_FILE_MODE ((mode_t)0777)
#define AVA1_CONSOLE_DIR_MODE ((mode_t)0777)

#define AVA1_PATH_CAP (2 * AVA1_MAX_PATH + 64) /* base (root + .ava-part) / entry path + .ava-part */

static inline uint64_t ava1_groups_of(uint64_t size) { return (size + AVA1_GROUP_LEN - 1) / AVA1_GROUP_LEN; }

/* A directory's entries are durable only once it is synced (SPEC.md §12.6). 0 or an errno;
 * a filesystem that cannot sync a directory (EINVAL, ENOTSUP) is not an error. */
int ava1_sync_dir(const char *dir);
void ava1_parent_of(const char *p, char *out, size_t cap);
/* mkdir -p of every directory above `path` (not `path` itself). 0 or -errno. */
int ava1_mkparents(const char *path);
/* mkdir -p of `path` itself too; with `sync`, the parent of every directory it creates is
 * synced so the new entry is durable. 0 or -errno. */
int ava1_mkdirs(const char *path, int sync);
/* The large-file state of `id`, allocated (descriptors -1) when missing. Caller holds j->mu
 * or owns the job alone. NULL when out of memory. */
ava1_lfile_t *ava1_lfile_get(ava1_job_t *j, uint32_t id);
/* Rebuilds the large-file index from j->lf (after the array is replaced or reindexed).
 * Caller holds j->mu or owns the job alone. */
void ava1_lflist_rebuild(ava1_job_t *j);
/* Forgets the index (the job's state is dropped or freed). */
void ava1_lflist_reset(ava1_job_t *j, int release);
/* Closes a large file's descriptors and gives their slots in the large-file budget back (a file with
 * none open: nothing). Caller holds j->mu or owns the job alone. `lf` is j->lf[id]. */
void ava1_lf_close_fds(ava1_job_t *j, uint32_t id, ava1_lfile_t *lf);
/* A directory that gained an entry; `id` names a file in it (for the test hook). */
typedef struct {
    char *dir; /* malloc'd; ava1_sync_dirset frees it */
    uint32_t id;
} ava1_dirent_t;
/* Syncs each distinct directory once (sorting `d`), calling the test hook at `hook_point`
 * (0: none) after each, and frees every string. 0, an errno, or -1 when the job is stopping. */
int ava1_sync_dirset(ava1_job_t *j, ava1_dirent_t *d, uint32_t n, int hook_point);
/* Tests (data cfg crash_at): stop the job dead, as a power cut would. */
void ava1_apply_crash(ava1_job_t *j);

#endif
