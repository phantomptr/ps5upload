/* AVA1 filesystem management methods (P3 Task 4): fs.list, fs.stat, fs.freespace, fs.mkdir, fs.rename,
 * fs.chmod, fs.read and fs.write.
 *
 * These are native (not the management handlers behind the capture sink): their bodies are typed
 * (SPEC.md section 7.5), and keeping them in one file that includes no runtime.c types lets the
 * host harness (ava1-ctest) run the very code the console runs. The path policy and the command
 * counter are the only things that stay in runtime.c; it hands them over through
 * mgmt_fs_set_policy() (runtime_mgmt_install()).
 *
 * Each runner has the mgmt_run_fn shape and is a `MGMT_N` line in mgmt_table.def. The error
 * causes are the the handlers' tokens (fs_list_dir_path_denied, fs_move_cross_mount, ...) so a
 * caller that matches on them keeps working.
 */
#ifndef PS5UPLOAD_MGMT_FS_H
#define PS5UPLOAD_MGMT_FS_H

#include <stdint.h>

#include "mgmt_rpc.h"

typedef struct {
    /* The writable-root allowlist (runtime.c is_path_allowed): fs.mkdir, fs.rename, fs.chmod and
     * fs.write act only on paths it accepts. */
    int (*write_allowed)(const char *path);
    /* The read policy: the allowlist, the avatar carve-out, and (unsafe_read = FSR_UNSAFE) the
     * read-only system partitions. fs.read acts only on paths it accepts. */
    int (*read_allowed)(const char *path, int unsafe_read);
    /* One successful command (the node's command_count). May be NULL. */
    void (*count)(void);
    /* Device id of the rename guard's destination directory (xdev_dev_fn, cross_device.h); NULL = stat(2). */
    int (*dev_of)(const char *path, unsigned long long *out);
    /* Device id of the rename's SOURCE: its own device, a link not followed; NULL = lstat(2). */
    int (*src_dev_of)(const char *path, unsigned long long *out);
} mgmt_fs_policy_t;

/* Installs the policy (copied). Until it is called every path is refused. */
void mgmt_fs_set_policy(const mgmt_fs_policy_t *p);

int mgmt_run_fs_list(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);   /* FsList -> FsListResult */
int mgmt_run_fs_stat(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);   /* FsPath -> FsStat */
int mgmt_run_fs_freespace(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx); /* FsPath -> FsFreeSpace */
/* The working margin kept back from writes on a drive of `total_bytes` (1/64th, at most 1 GiB). */
uint64_t mgmt_fs_reserve_for(uint64_t total_bytes);
int mgmt_run_fs_mkdir(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);  /* FsMkdir -> empty */
int mgmt_run_fs_rename(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx); /* FsRename -> empty */
int mgmt_run_fs_chmod(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);  /* FsChmod -> empty */
int mgmt_run_fs_read(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);   /* FsRead -> FsReadResult */
int mgmt_run_fs_write(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx);  /* FsWrite -> empty */

/* The suffix of the temporary file a chunked fs.write fills (SPEC.md section 7.5). */
#define MGMT_FS_TMP_SUFFIX ".ps5upload.tmp"

#endif
