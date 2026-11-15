#ifndef PS5UPLOAD_HELPER_UNINSTALL_H
#define PS5UPLOAD_HELPER_UNINSTALL_H

/* "Remove PS5Upload from this PS5" (node.uninstall, method 200).
 *
 * The helper takes away what it put on the console itself: the Minion tile (and Minion's own
 * files), icons it hid are shown again, then it shuts down and, as its very last step (main.c,
 * after every service stopped writing), deletes its folders: /data/ps5upload (the save backups
 * in it only when asked), /user/data/ps5upload and /data/ps5upload_startup.log. Stored copies of
 * the helper in loaders (Payload Manager, autoloader folders) are the PC's job: it knows them.
 *
 * Nothing mounted is ever gone into: the request is refused while anything is mounted at or
 * under those folders (an image, a share, a save), and the delete never enters a folder on
 * another filesystem either, so a game mounted there cannot lose its files. */

#include <stddef.h>
#include <sys/types.h>

#define HU_ROOT "/data/ps5upload"
#define HU_USER_ROOT "/user/data/ps5upload"
#define HU_STARTUP_LOG "/data/ps5upload_startup.log"
/* The final delete's own record, outside the folders it deletes: each top-level entry before it
 * goes (synced), then the counts, so a delete that stalls or a console that goes down shows where.
 * Left in place; the next helper start leaves it alone. */
#define HU_WIPE_LOG "/data/ps5upload_uninstall.log"
#define HU_BACKUPS "/data/ps5upload/backups"
/* The account's state from before the offline PSN sign-in (profile.c): the only way to undo that
 * sign-in, so it always stays (the user's data, not PS5Upload's). Same path as
 * PROFILE_NP_BACKUP_ROOT. */
#define HU_NP_BACKUP "/data/ps5upload/np-backup"

typedef struct {
    unsigned removed; /* files and folders deleted */
    unsigned kept;    /* left on purpose: a kept path, another filesystem */
    unsigned failed;  /* could not be deleted */
} hu_counts_t;

/* rm -r of `path`, staying on its filesystem: a folder whose device differs from `path`'s own
 * is left alone (counted kept), and so is any path equal to one of `keep`. A missing `path` is
 * nothing to do. Folders holding something kept stay (not a failure). */
void hu_remove_tree(const char *path, const char *const *keep, size_t nkeep, hu_counts_t *c);

/* 1 when mount point `mp` is `root` or inside it. */
int hu_mount_under(const char *mp, const char *root);

#define HU_KEEP_PATH 256
/* The folders of ours that something is mounted FROM (the Minion tile's nullfs at
 * /system_ex/app/PSUP00001 is /data/ps5upload/tile): never deleted, nor gone into, while that
 * mount lives: deleting the folder under a live bind is never safe. For each source in `froms` under HU_ROOT or HU_USER_ROOT (the same folder under
 * two names), both names go into `out` (at most `cap`). Returns how many were written. */
size_t hu_source_keeps(const char *const *froms, size_t n, char (*out)[HU_KEEP_PATH], size_t cap);

/* Mount points at or under HU_ROOT / HU_USER_ROOT, as a JSON array of strings written into
 * `out` (at most `cap` bytes, always terminated). Returns how many there are, -1 when the
 * mount table cannot be read. Console only. */
int hu_mounts_in_use(char *out, size_t cap);

/* Shows every icon the helper hid (its hidden list) and forgets the list. Returns how many were
 * shown. Console only. */
int hu_show_hidden_icons(void);

/* Ask for the final delete when this instance exits (keep_backups: HU_BACKUPS stays). */
void hu_request(int keep_backups);
int hu_requested(void);

/* The final delete (main.c, last), recorded in HU_WIPE_LOG. HU_NP_BACKUP always stays, and so does any folder of ours
 * a live mount comes from (hu_source_keeps). */
void hu_wipe(void);

#endif
