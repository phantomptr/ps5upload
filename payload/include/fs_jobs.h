/* Filesystem operations that run as AVA1 jobs (P3 Task 5): recursive delete, recursive chmod,
 * BLAKE3 file hash and CRC-32 file checksum, plus the tree walkers the management handlers in
 * runtime.c share with them (rm_rf_op, chmod_rf, recursive_size_op delegate here).
 *
 * Plain POSIX and BLAKE3, no runtime.c types, so the host test harness (ava1-ctest) runs
 * the real code against temporary trees. Policy comes from ava1_data_cfg() (may_write,
 * may_read, same_device), the same hooks the data plane uses. */
#ifndef PS5UPLOAD_FS_JOBS_H
#define PS5UPLOAD_FS_JOBS_H

#include <stddef.h>
#include <stdint.h>

/* Progress and cancel for a walk. `cancelled` is polled between directory entries; `file`
 * is called once per removed or visited non-directory with its size (regular files; 0 for
 * a link). Either may be NULL, and so may the whole struct. */
typedef struct {
    int (*cancelled)(void *arg);
    void (*file)(void *arg, uint64_t bytes);
    void *arg;
    /* An entry the walk could not handle: `path` and what happened ("unlink", "rmdir", "opendir",
     * "chmod", "mount": a different device than the walk's root, skipped, never entered). Called
     * once per entry that itself failed (a folder that is only non-empty because a child failed
     * is not reported again), so a caller can name the first and count the rest. */
    void (*failed)(void *arg, const char *path, const char *what);
} fsj_hooks_t;

/* Recursively removes `path`: 0 ok (an already-missing path is ok), -1 error (keeps going,
 * best effort), -2 cancelled (the top directory is then left in place). */
int fsj_rm_rf(const char *path, int depth, const fsj_hooks_t *h);
/* Recursive chmod, the top first. 0, -1 error, -2 cancelled. */
int fsj_chmod_rf(const char *path, unsigned mode, int depth, const fsj_hooks_t *h);
/* Adds the regular files' bytes to *bytes and the non-directories to *files. 0, -1, -2. */
int fsj_tree_size(const char *path, uint64_t *bytes, uint64_t *files, int depth, const fsj_hooks_t *h);

/* The walkers never cross into another device than the root's: a nested mount (a USB drive
 * under a game folder, an image mounted inside it) is skipped, reported through `failed("mount")`
 * and counted as an error, so deleting a tree can never empty a volume mounted below it. */

/* Copies `src` over `dst` atomically: written to a temporary file next to `dst` (same folder,
 * so the final rename never crosses a device), fsynced, given src's times, then renamed into
 * place. `dst` is untouched until the copy is complete; a failure or a cancel removes only the
 * temporary file. `h->file` is called per block with its size. 0, -1, or -2 cancelled. */
int fsj_copy_atomic(const char *src, const char *dst, const fsj_hooks_t *h);

/* Normalises an absolute path in place: collapses `//` and `/./`, drops a trailing `/` and `/.`,
 * and refuses `..`. 0, or -1 (relative, or a `..` component, or empty). */
int fsj_normalize_path(char *path);
/* 1 when a normalised path is a root or a mount-point-like top folder: `/`, `/data`, `/user`, `/mnt`,
 * `/mnt/usb0`, `/mnt/ext1` (anything under `/mnt` needs three components, anything else two). */
int fsj_path_is_root(const char *path);

/* Registers AVA1_JOB_OP_DELETE, _CHMOD_R, _HASH and _CRC32 with ava1_op.c. */
void fsj_register_ops(void);

/* The request-body helpers the operations use (the legacy JSON bodies, read as C text):
 * the string at top-level key `key`, unescaped (1 found, 0 absent or too long), and the
 * unsigned integer or boolean at `key` (1 found; `true` is 1). */
int fsj_json_str(const char *json, const char *key, char *out, size_t cap);
int fsj_json_u64(const char *json, const char *key, uint64_t *out);

/* Tests only: every visited file waits this long (a slow disk), so a test can watch
 * progress and cancel mid-walk. */
extern uint32_t fsj_test_file_delay_us;
/* Tests only: a folder or file with this base name reports a different st_dev than its parent
 * (a mount point), so the walkers' device guard can be exercised without a mount. NULL = off. */
extern const char *fsj_test_cross_name;

#endif
